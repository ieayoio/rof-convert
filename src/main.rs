//! rof-convert：基于 LibreOffice 的企业级文档转换 HTTP 服务。

mod api;
mod config;
mod engine;
mod error;
mod formats;
mod jobs;
mod middleware;
mod response;
mod routes;
mod state;
mod worker;

use std::process::ExitCode;

use tokio::{net::TcpListener, signal};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use crate::{
    config::{Config, LogFormat},
    formats::Capabilities,
    state::AppState,
};

#[tokio::main]
async fn main() -> ExitCode {
    let config = match Config::load() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("configuration error: {error}");
            return ExitCode::FAILURE;
        }
    };
    init_tracing(&config);

    match run(config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = %error, "server terminated unexpectedly");
            ExitCode::FAILURE
        }
    }
}

async fn run(mut config: Config) -> Result<(), Box<dyn std::error::Error>> {
    // 临时目录是最常见的启动失败原因（目录不存在、属主不是运行用户、只读挂载），
    // 这里补上路径与配置来源，避免只剩一个裸的 `Permission denied`。
    prepare_temp_dir(&mut config).await.map_err(|error| {
        format!(
            "failed to prepare temp dir `{}` from `{}`: {error}",
            config.temp_dir.display(),
            config.config_file.display()
        )
    })?;
    cleanup_stale_workspaces(&config).await;

    let capabilities = match engine::find_executable(&config.soffice_bin) {
        Some(binary) => {
            tracing::info!(binary = %binary.display(), "LibreOffice binary located");
            engine::detect_capabilities(&config).await
        }
        None => {
            tracing::warn!(
                binary = config.soffice_bin,
                "LibreOffice binary not found; readiness checks will report DOWN"
            );
            Capabilities::NONE
        }
    };
    tracing::info!(
        markdown = capabilities.markdown,
        "LibreOffice capabilities detected"
    );

    let listener = TcpListener::bind(config.bind_addr).await?;
    let address = listener.local_addr()?;
    let state = AppState::new(config, capabilities);
    worker::spawn_janitor(state.clone());
    let app = routes::build_router(state.clone());

    tracing::info!(
        address = %address,
        version = env!("CARGO_PKG_VERSION"),
        environment = %state.config.environment,
        config_file = %state.config.config_file.display(),
        auth_enabled = state.config.auth_enabled(),
        docs_enabled = state.config.docs_enabled,
        markdown_enabled = state.capabilities.markdown,
        max_concurrency = state.config.max_concurrency,
        max_upload_mb = state.config.max_upload_mb(),
        temp_dir = %state.config.temp_dir.display(),
        "rof-convert started"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    worker::shutdown(&state).await;
    tracing::info!("rof-convert stopped");
    Ok(())
}

fn init_tracing(config: &Config) {
    let filter = EnvFilter::try_new(&config.log_filter).unwrap_or_else(|error| {
        eprintln!(
            "invalid log filter `{}`: {error}; falling back to `rof_convert=info`",
            config.log_filter
        );
        EnvFilter::new("rof_convert=info")
    });
    match config.log_format {
        LogFormat::Json => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .flatten_event(true)
            .init(),
        LogFormat::Text => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}

async fn prepare_temp_dir(config: &mut Config) -> std::io::Result<()> {
    tokio::fs::create_dir_all(&config.temp_dir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // 容器 bind mount / 共享卷常由 root 创建，运行用户不是属主时 chmod 会返回 EPERM。
        // 权限收紧只是加固，真正的判据是目录可写，所以这里降级为警告，由下面的写探测把关。
        if let Err(error) =
            tokio::fs::set_permissions(&config.temp_dir, std::fs::Permissions::from_mode(0o700))
                .await
        {
            tracing::warn!(
                path = %config.temp_dir.display(),
                error = %error,
                "failed to tighten temp dir permissions to 0700; keeping the existing mode"
            );
        }
    }
    config.temp_dir = tokio::fs::canonicalize(&config.temp_dir).await?;

    // 主动探测可写性：目录不可写时在启动阶段就报错，而不是等到第一次转换才失败。
    let probe = config
        .temp_dir
        .join(format!(".write-probe-{}", Uuid::new_v4()));
    tokio::fs::write(&probe, b"").await?;
    let _ = tokio::fs::remove_file(&probe).await;
    Ok(())
}

/// 清理上次进程异常退出后遗留的任务目录，避免临时磁盘持续增长。
async fn cleanup_stale_workspaces(config: &Config) {
    let retention = config.job_ttl.saturating_add(config.conversion_timeout);
    let Ok(mut entries) = tokio::fs::read_dir(&config.temp_dir).await else {
        return;
    };
    let mut removed = 0_usize;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let is_job_directory = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| Uuid::parse_str(name).ok())
            .is_some();
        if !is_job_directory {
            continue;
        }
        let stale = entry
            .metadata()
            .await
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > retention);
        if !stale {
            continue;
        }
        match tokio::fs::remove_dir_all(&path).await {
            Ok(()) => removed += 1,
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    "failed to remove stale job workspace"
                );
            }
            Err(_) => {}
        }
    }
    if removed > 0 {
        tracing::info!(count = removed, "removed stale job workspaces");
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C signal handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}

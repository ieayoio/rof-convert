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
    prepare_temp_dir(&mut config).await?;
    cleanup_stale_workspaces(&config).await;

    if let Some(binary) = engine::find_executable(&config.soffice_bin) {
        tracing::info!(binary = %binary.display(), "LibreOffice binary located");
    } else {
        tracing::warn!(
            binary = config.soffice_bin,
            "LibreOffice binary not found; readiness checks will report DOWN"
        );
    }

    let listener = TcpListener::bind(config.bind_addr).await?;
    let address = listener.local_addr()?;
    let state = AppState::new(config);
    worker::spawn_janitor(state.clone());
    let app = routes::build_router(state.clone());

    tracing::info!(
        address = %address,
        version = env!("CARGO_PKG_VERSION"),
        environment = %state.config.environment,
        config_file = %state.config.config_file.display(),
        auth_enabled = state.config.auth_enabled(),
        docs_enabled = state.config.docs_enabled,
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
        tokio::fs::set_permissions(&config.temp_dir, std::fs::Permissions::from_mode(0o700))
            .await?;
    }
    config.temp_dir = tokio::fs::canonicalize(&config.temp_dir).await?;
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

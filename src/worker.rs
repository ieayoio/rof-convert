//! 转换任务执行器：并发控制、超时取消与工作目录清理。

use std::{
    panic::AssertUnwindSafe,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::Utc;
use futures_util::FutureExt;

use crate::{
    engine, formats,
    jobs::{Job, JobFailure, JobResult},
    state::AppState,
};

pub fn spawn(state: AppState, job: Arc<Job>) {
    let panic_job = job.clone();
    tokio::spawn(async move {
        if AssertUnwindSafe(run(state, job))
            .catch_unwind()
            .await
            .is_err()
        {
            tracing::error!(job_id = %panic_job.id, "conversion worker panicked");
            if panic_job.fail(JobFailure {
                code: "INTERNAL_ERROR".to_string(),
                message: "internal conversion error".to_string(),
            }) {
                cleanup_work_dir(&panic_job.work_dir).await;
            }
        }
    });
}

async fn run(state: AppState, job: Arc<Job>) {
    let permit = tokio::select! {
        permit = state.permits.clone().acquire_owned() => permit,
        _ = job.cancel.cancelled() => {
            cleanup_work_dir(&job.work_dir).await;
            return;
        }
    };
    let permit = match permit {
        Ok(permit) => permit,
        Err(_) => {
            job.fail(JobFailure {
                code: "SERVICE_UNAVAILABLE".to_string(),
                message: "conversion service is shutting down".to_string(),
            });
            cleanup_work_dir(&job.work_dir).await;
            return;
        }
    };

    if job.cancel.is_cancelled() || !job.mark_running() {
        drop(permit);
        cleanup_work_dir(&job.work_dir).await;
        return;
    }

    tracing::info!(
        job_id = %job.id,
        request_id = %job.request_id,
        source = job.source_format,
        target = %job.target_format,
        "conversion job started"
    );

    let started = Instant::now();
    let profile_dir = job.work_dir.join("profile");
    let outcome = engine::convert(
        &state.config,
        &job.input_path,
        &job.output_dir,
        &profile_dir,
        job.filter,
        &job.target_format,
        &job.cancel,
    )
    .await;

    let mut remove_workspace = false;
    match outcome {
        Ok(_) if job.cancel.is_cancelled() => {
            tracing::info!(job_id = %job.id, "conversion discarded because the job was cancelled");
            remove_workspace = true;
        }
        Ok(outcome) => {
            let result = JobResult {
                file_name: formats::output_file_name(&job.source_file_name, &job.target_format),
                file_path: outcome.file_path,
                size_bytes: outcome.size_bytes,
                sha256: outcome.sha256,
                content_type: formats::content_type(&job.target_format),
            };
            if job.succeed(result) {
                tracing::info!(
                    job_id = %job.id,
                    duration_ms = started.elapsed().as_millis() as u64,
                    size_bytes = outcome.size_bytes,
                    "conversion job succeeded"
                );
            } else {
                remove_workspace = true;
            }
        }
        Err(error) => {
            if job.cancel.is_cancelled() {
                tracing::info!(job_id = %job.id, "conversion job cancelled");
            } else {
                tracing::warn!(
                    job_id = %job.id,
                    code = error.code.as_str(),
                    error = %error.message,
                    "conversion job failed"
                );
                job.fail(JobFailure {
                    code: error.code.as_str().to_string(),
                    message: error.message,
                });
            }
            remove_workspace = true;
        }
    }

    drop(permit);
    if remove_workspace {
        cleanup_work_dir(&job.work_dir).await;
    } else {
        // 转换结果需保留至下载/过期；源文件与隔离配置目录可立即释放。
        remove_file_quietly(&job.input_path).await;
        remove_dir_quietly(&profile_dir).await;
    }
}

pub fn spawn_janitor(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let expired = state.jobs.remove_expired(state.config.job_ttl, Utc::now());
            if !expired.is_empty() {
                tracing::info!(count = expired.len(), "expired conversion jobs removed");
                for job in expired {
                    cleanup_work_dir(&job.work_dir).await;
                }
            }
        }
    });
}

pub async fn shutdown(state: &AppState) {
    let jobs = state.jobs.drain();
    if jobs.is_empty() {
        return;
    }
    tracing::info!(
        count = jobs.len(),
        "cancelling conversion jobs before shutdown"
    );
    for job in &jobs {
        job.cancel.cancel();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    for job in jobs {
        cleanup_work_dir(&job.work_dir).await;
    }
}

pub async fn cleanup_work_dir(path: &Path) {
    if let Err(error) = tokio::fs::remove_dir_all(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            path = %path.display(),
            error = %error,
            "failed to clean up job workspace"
        );
    }
}

async fn remove_file_quietly(path: &Path) {
    if let Err(error) = tokio::fs::remove_file(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::debug!(path = %path.display(), error = %error, "failed to remove temporary file");
    }
}

async fn remove_dir_quietly(path: &Path) {
    if let Err(error) = tokio::fs::remove_dir_all(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::debug!(path = %path.display(), error = %error, "failed to remove temporary directory");
    }
}

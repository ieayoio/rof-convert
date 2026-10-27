//! 转换任务模型与内存任务仓储。

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, RwLock},
    time::Duration,
};

use chrono::{DateTime, TimeDelta, Utc};
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl JobStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobFailure {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct JobResult {
    pub file_name: String,
    pub file_path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
    pub content_type: String,
}

#[derive(Debug, Clone)]
pub struct JobState {
    pub status: JobStatus,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub failure: Option<JobFailure>,
    pub result: Option<JobResult>,
}

impl JobState {
    fn queued() -> Self {
        Self {
            status: JobStatus::Queued,
            started_at: None,
            finished_at: None,
            failure: None,
            result: None,
        }
    }
}

#[derive(Debug)]
pub struct Job {
    pub id: Uuid,
    pub request_id: String,
    pub source_file_name: String,
    pub source_format: &'static str,
    pub source_size_bytes: u64,
    pub target_format: String,
    pub filter: &'static str,
    pub created_at: DateTime<Utc>,
    pub input_path: PathBuf,
    pub output_dir: PathBuf,
    pub work_dir: PathBuf,
    pub cancel: CancellationToken,
    state: Mutex<JobState>,
}

pub struct JobSpec {
    pub id: Uuid,
    pub request_id: String,
    pub source_file_name: String,
    pub source_format: &'static str,
    pub source_size_bytes: u64,
    pub target_format: String,
    pub filter: &'static str,
    pub input_path: PathBuf,
    pub output_dir: PathBuf,
    pub work_dir: PathBuf,
}

impl Job {
    pub fn new(spec: JobSpec) -> Self {
        Self {
            id: spec.id,
            request_id: spec.request_id,
            source_file_name: spec.source_file_name,
            source_format: spec.source_format,
            source_size_bytes: spec.source_size_bytes,
            target_format: spec.target_format,
            filter: spec.filter,
            created_at: Utc::now(),
            input_path: spec.input_path,
            output_dir: spec.output_dir,
            work_dir: spec.work_dir,
            cancel: CancellationToken::new(),
            state: Mutex::new(JobState::queued()),
        }
    }

    pub fn state(&self) -> JobState {
        self.lock_state().clone()
    }

    pub fn mark_running(&self) -> bool {
        let mut state = self.lock_state();
        if state.status != JobStatus::Queued {
            return false;
        }
        state.status = JobStatus::Running;
        state.started_at = Some(Utc::now());
        true
    }

    pub fn succeed(&self, result: JobResult) -> bool {
        let mut state = self.lock_state();
        if state.status != JobStatus::Running {
            return false;
        }
        state.status = JobStatus::Succeeded;
        state.finished_at = Some(Utc::now());
        state.result = Some(result);
        true
    }

    pub fn fail(&self, failure: JobFailure) -> bool {
        let mut state = self.lock_state();
        if state.status.is_terminal() {
            return false;
        }
        state.status = JobStatus::Failed;
        state.finished_at = Some(Utc::now());
        state.failure = Some(failure);
        true
    }

    pub fn view(&self, retention: Duration) -> JobView {
        let state = self.state();
        let expires_at = state.finished_at.and_then(|finished_at| {
            TimeDelta::from_std(retention)
                .ok()
                .map(|delta| finished_at + delta)
        });
        let download_url = state
            .result
            .as_ref()
            .map(|_| format!("/api/v1/conversions/{}/download", self.id));

        JobView {
            job_id: self.id.to_string(),
            status: state.status,
            source: SourceView {
                file_name: self.source_file_name.clone(),
                format: self.source_format,
                size_bytes: self.source_size_bytes,
            },
            target: TargetView {
                format: self.target_format.clone(),
            },
            result: state.result.map(|result| ResultView {
                file_name: result.file_name,
                content_type: result.content_type,
                size_bytes: result.size_bytes,
                sha256: result.sha256,
            }),
            error: state.failure,
            created_at: self.created_at,
            started_at: state.started_at,
            completed_at: state.finished_at,
            expires_at,
            status_url: format!("/api/v1/conversions/{}", self.id),
            download_url,
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, JobState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    pub job_id: String,
    pub status: JobStatus,
    pub source: SourceView,
    pub target: TargetView,
    pub result: Option<ResultView>,
    pub error: Option<JobFailure>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub status_url: String,
    pub download_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceView {
    pub file_name: String,
    pub format: &'static str,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetView {
    pub format: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultView {
    pub file_name: String,
    pub content_type: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Default)]
pub struct JobStore {
    jobs: Arc<RwLock<HashMap<Uuid, Arc<Job>>>>,
}

impl JobStore {
    pub fn insert(&self, job: Arc<Job>) {
        self.write().insert(job.id, job);
    }

    pub fn get(&self, id: Uuid) -> Option<Arc<Job>> {
        self.read().get(&id).cloned()
    }

    pub fn remove(&self, id: Uuid) -> Option<Arc<Job>> {
        self.write().remove(&id)
    }

    pub fn active_count(&self) -> usize {
        self.read()
            .values()
            .filter(|job| !job.state().status.is_terminal())
            .count()
    }

    /// 移除超过保留期的终态任务，返回被移除的任务用于清理磁盘文件。
    pub fn remove_expired(&self, retention: Duration, now: DateTime<Utc>) -> Vec<Arc<Job>> {
        let mut jobs = self.write();
        let expired: Vec<Uuid> = jobs
            .iter()
            .filter_map(|(id, job)| {
                let finished_at = job.state().finished_at?;
                let retention = TimeDelta::from_std(retention).ok()?;
                (finished_at + retention <= now).then_some(*id)
            })
            .collect();
        expired
            .into_iter()
            .filter_map(|id| jobs.remove(&id))
            .collect()
    }

    pub fn drain(&self) -> Vec<Arc<Job>> {
        self.write().drain().map(|(_, job)| job).collect()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<Uuid, Arc<Job>>> {
        self.jobs
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<Uuid, Arc<Job>>> {
        self.jobs
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_job() -> Arc<Job> {
        Arc::new(Job::new(JobSpec {
            id: Uuid::new_v4(),
            request_id: "req-1".to_string(),
            source_file_name: "report.docx".to_string(),
            source_format: "docx",
            source_size_bytes: 1024,
            target_format: "pdf".to_string(),
            filter: "pdf:writer_pdf_Export",
            input_path: PathBuf::from("/tmp/job/input.docx"),
            output_dir: PathBuf::from("/tmp/job/out"),
            work_dir: PathBuf::from("/tmp/job"),
        }))
    }

    fn sample_result() -> JobResult {
        JobResult {
            file_name: "report.pdf".to_string(),
            file_path: PathBuf::from("/tmp/job/out/report.pdf"),
            size_bytes: 2048,
            sha256: "hash".to_string(),
            content_type: "application/pdf".to_string(),
        }
    }

    #[test]
    fn enforces_state_transitions() {
        let job = sample_job();
        assert!(!job.succeed(sample_result()));
        assert!(job.mark_running());
        assert!(!job.mark_running());
        assert!(job.succeed(sample_result()));
        assert!(!job.fail(JobFailure {
            code: "CONVERSION_FAILED".to_string(),
            message: "boom".to_string(),
        }));
    }

    #[test]
    fn builds_job_view_with_download_url() {
        let job = sample_job();
        job.mark_running();
        job.succeed(sample_result());
        let view = job.view(Duration::from_secs(3600));
        let expected_url = format!("/api/v1/conversions/{}/download", job.id);
        assert_eq!(view.status, JobStatus::Succeeded);
        assert_eq!(view.download_url.as_deref(), Some(expected_url.as_str()));
        assert!(view.expires_at.is_some());
    }
}

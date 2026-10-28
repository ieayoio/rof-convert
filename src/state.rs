//! 应用共享状态。

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::Semaphore;

use crate::{config::Config, formats::Capabilities, jobs::JobStore};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    /// 启动时探测到的 LibreOffice 能力，决定对外暴露哪些格式。
    pub capabilities: Capabilities,
    pub jobs: JobStore,
    pub permits: Arc<Semaphore>,
    pub started_at: DateTime<Utc>,
}

impl AppState {
    pub fn new(config: Config, capabilities: Capabilities) -> Self {
        let permits = Arc::new(Semaphore::new(config.max_concurrency));
        Self {
            config: Arc::new(config),
            capabilities,
            jobs: JobStore::default(),
            permits,
            started_at: Utc::now(),
        }
    }
}

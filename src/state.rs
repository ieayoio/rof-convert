//! 应用共享状态。

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::Semaphore;

use crate::{config::Config, jobs::JobStore};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub jobs: JobStore,
    pub permits: Arc<Semaphore>,
    pub started_at: DateTime<Utc>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let permits = Arc::new(Semaphore::new(config.max_concurrency));
        Self {
            config: Arc::new(config),
            jobs: JobStore::default(),
            permits,
            started_at: Utc::now(),
        }
    }
}

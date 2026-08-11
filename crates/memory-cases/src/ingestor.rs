use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::time::sleep;

use crate::service::RagService;

pub async fn run(service: Arc<RagService>, poll_ms: u64) -> Result<()> {
    let poll_interval = Duration::from_millis(poll_ms.max(100));
    loop {
        match service.run_next_ingestion_task().await {
            Ok(true) => {}
            Ok(false) => sleep(poll_interval).await,
            Err(_error) => {
                tracing::error!(
                    event = "ram_a.case.ingestion.failed",
                    error_kind = "task",
                    retriable = true
                );
                sleep(poll_interval).await;
            }
        }
    }
}

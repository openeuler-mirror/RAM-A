use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::time::sleep;

use crate::service::{
    ingestion_error_retriable, observable_error_kind, observable_error_summary, RagService,
};

pub async fn run(service: Arc<RagService>, poll_ms: u64) -> Result<()> {
    let poll_interval = Duration::from_millis(poll_ms.max(100));
    loop {
        match service.run_next_ingestion_task().await {
            Ok(true) => {}
            Ok(false) => sleep(poll_interval).await,
            Err(error) => {
                tracing::error!(
                    event = "ram_a.case.ingestion.failed",
                    error_kind = observable_error_kind(&error),
                    error = %observable_error_summary(&error),
                    retriable = ingestion_error_retriable(&error)
                );
                sleep(poll_interval).await;
            }
        }
    }
}

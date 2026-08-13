use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::service::RagService;

/// Continuously consumes pending case ingestion tasks until cancellation.
///
/// This is designed to run as a Tokio task inside the owning application; it
/// does not create a process or bind a network listener.
pub async fn run_until_cancelled(
    service: Arc<RagService>,
    poll_ms: u64,
    cancellation_token: CancellationToken,
) {
    let poll_interval = Duration::from_millis(poll_ms.max(100));
    loop {
        if cancellation_token.is_cancelled() {
            return;
        }
        match service.run_next_ingestion_task().await {
            Ok(true) => {}
            Ok(false) => sleep_or_cancel(poll_interval, &cancellation_token).await,
            Err(error) => {
                eprintln!("case ingestion task failed: {error}");
                if let Err(recovery_error) = service.recover_interrupted_ingestion_tasks() {
                    eprintln!("case ingestion task recovery failed: {recovery_error}");
                }
                sleep_or_cancel(poll_interval, &cancellation_token).await;
            }
        }
    }
}

async fn sleep_or_cancel(duration: Duration, cancellation_token: &CancellationToken) {
    tokio::select! {
        _ = cancellation_token.cancelled() => {}
        _ = tokio::time::sleep_until(Instant::now() + duration) => {}
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::run_until_cancelled;
    use crate::model::{CreateDatasetRequest, CreateDocumentFileRequest};
    use crate::{build_service, CaseServiceOptions, EmbeddingProviderKind};

    #[tokio::test]
    async fn worker_processes_pending_tasks_and_stops_on_cancellation() {
        let temp = TempDir::new().unwrap();
        let service = build_service(&CaseServiceOptions {
            rag_store: temp.path().join("cases.sqlite"),
            memory_store: temp.path().join("cases-index.sqlite"),
            embedding_provider: EmbeddingProviderKind::Hash,
            embedding_api_key_env: "UNUSED_CASE_EMBEDDING_KEY".to_string(),
            embedding_base_url: "http://127.0.0.1:1/v1".to_string(),
            embedding_model: "hash".to_string(),
            embedding_dimensions: 32,
            chunk_size: 32,
            summary_llm_model: None,
            summary_llm_api_key_env: "UNUSED_CASE_SUMMARY_KEY".to_string(),
            summary_llm_base_url: "http://127.0.0.1:1/v1".to_string(),
            summary_llm_timeout_ms: 1_000,
        })
        .unwrap();
        service
            .create_dataset(CreateDatasetRequest {
                id: Some("ops".to_string()),
                name: "Operations".to_string(),
                description: None,
            })
            .unwrap();
        let created = service
            .create_document(
                "ops",
                CreateDocumentFileRequest {
                    id: Some("document-1".to_string()),
                    task_id: Some("task-1".to_string()),
                    name: "dns.md".to_string(),
                    file_name: "dns.md".to_string(),
                    mime_type: Some("text/markdown".to_string()),
                    bytes: b"# DNS failure\n\nFlush the local resolver cache.".to_vec(),
                },
            )
            .await
            .unwrap();

        let cancellation = CancellationToken::new();
        let worker = tokio::spawn(run_until_cancelled(
            service.clone(),
            100,
            cancellation.clone(),
        ));

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let task = service.get_task(&created.task_id).unwrap().unwrap();
                if task.status == "completed" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("background ingestion should complete");

        cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .expect("worker should stop promptly")
            .expect("worker task should not panic");
        let chunks = service.list_chunks("ops", "document-1").unwrap();
        assert!(!chunks.chunks.is_empty());
    }
}

use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    is_business_database_missing, is_case_index_database_missing,
    service::{
        ingestion_error_retriable, observable_error_kind, observable_error_summary,
        CaseStorageAvailability, RagService,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StorageFailureKind {
    BusinessDatabaseMissing,
    IndexDatabaseMissing,
}

impl StorageFailureKind {
    fn from_error(error: &anyhow::Error) -> Option<Self> {
        if is_business_database_missing(error) {
            Some(Self::BusinessDatabaseMissing)
        } else if is_case_index_database_missing(error) {
            Some(Self::IndexDatabaseMissing)
        } else {
            None
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::BusinessDatabaseMissing => "business_database_missing",
            Self::IndexDatabaseMissing => "index_database_missing",
        }
    }

    fn error_code(self) -> &'static str {
        match self {
            Self::BusinessDatabaseMissing => crate::error::CASE_BUSINESS_DATABASE_MISSING,
            Self::IndexDatabaseMissing => crate::error::CASE_INDEX_DATABASE_MISSING,
        }
    }

    fn error_summary(self) -> &'static str {
        match self {
            Self::BusinessDatabaseMissing => "case business database is missing",
            Self::IndexDatabaseMissing => "case index database is missing",
        }
    }
}

#[derive(Default)]
struct StorageFailureTracker {
    business_database_failures: u32,
    index_database_failures: u32,
}

struct StorageFailureObservation {
    consecutive_failures: u32,
    newly_missing: bool,
    should_log: bool,
}

impl StorageFailureTracker {
    fn record(&mut self, kind: StorageFailureKind) -> StorageFailureObservation {
        let failures = self.failures_mut(kind);
        let newly_missing = *failures == 0;
        *failures = failures.saturating_add(1);
        StorageFailureObservation {
            consecutive_failures: *failures,
            newly_missing,
            should_log: newly_missing || should_log_storage_failure(*failures),
        }
    }

    fn recover(&mut self, kind: StorageFailureKind) -> Option<u32> {
        let failures = self.failures_mut(kind);
        (*failures > 0).then(|| std::mem::take(failures))
    }

    fn failures_mut(&mut self, kind: StorageFailureKind) -> &mut u32 {
        match kind {
            StorageFailureKind::BusinessDatabaseMissing => {
                &mut self.business_database_failures
            }
            StorageFailureKind::IndexDatabaseMissing => &mut self.index_database_failures,
        }
    }
}

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
    let mut storage_failures = StorageFailureTracker::default();
    loop {
        if cancellation_token.is_cancelled() {
            return;
        }

        match service.storage_availability() {
            Ok(availability) => {
                observe_storage_availability(
                    &mut storage_failures,
                    availability,
                    poll_interval,
                );
                if !availability.business_database_exists
                    || !availability.index_database_exists
                {
                    sleep_or_cancel(poll_interval, &cancellation_token).await;
                    continue;
                }
            }
            Err(error) => {
                tracing::error!(
                    event = "ram_a.case.ingestion.failed",
                    stage = "storage_check",
                    error_kind = observable_error_kind(&error),
                    error = %observable_error_summary(&error),
                    retriable = ingestion_error_retriable(&error),
                    retry_delay_ms = poll_interval.as_millis() as u64
                );
                sleep_or_cancel(poll_interval, &cancellation_token).await;
                continue;
            }
        }

        match service.run_next_ingestion_task().await {
            Ok(true) => {}
            Ok(false) => {
                sleep_or_cancel(poll_interval, &cancellation_token).await;
            }
            Err(error) => {
                if let Some(kind) = StorageFailureKind::from_error(&error) {
                    let observation = storage_failures.record(kind);
                    if observation.should_log {
                        log_storage_missing(kind, &observation, poll_interval);
                    }
                } else {
                    tracing::error!(
                        event = "ram_a.case.ingestion.failed",
                        stage = "task",
                        error_kind = observable_error_kind(&error),
                        error = %observable_error_summary(&error),
                        retriable = ingestion_error_retriable(&error)
                    );
                    if let Err(recovery_error) = service.recover_interrupted_ingestion_tasks() {
                        tracing::error!(
                            event = "ram_a.case.ingestion.failed",
                            stage = "recovery",
                            error_kind = observable_error_kind(&recovery_error),
                            error = %observable_error_summary(&recovery_error),
                            retriable = ingestion_error_retriable(&recovery_error)
                        );
                    }
                }
                sleep_or_cancel(poll_interval, &cancellation_token).await;
            }
        }
    }
}

fn observe_storage_availability(
    storage_failures: &mut StorageFailureTracker,
    availability: CaseStorageAvailability,
    poll_interval: Duration,
) {
    for (kind, exists) in [
        (
            StorageFailureKind::BusinessDatabaseMissing,
            availability.business_database_exists,
        ),
        (
            StorageFailureKind::IndexDatabaseMissing,
            availability.index_database_exists,
        ),
    ] {
        if exists {
            if let Some(previous_consecutive_failures) = storage_failures.recover(kind) {
                log_storage_recovered(kind, previous_consecutive_failures);
            }
        } else {
            let observation = storage_failures.record(kind);
            if observation.should_log {
                log_storage_missing(kind, &observation, poll_interval);
            }
        }
    }
}

fn log_storage_missing(
    kind: StorageFailureKind,
    observation: &StorageFailureObservation,
    poll_interval: Duration,
) {
    tracing::error!(
        event = "ram_a.case.ingestion.paused",
        stage = "storage",
        error_code = kind.error_code(),
        error_kind = kind.as_str(),
        error = kind.error_summary(),
        retriable = true,
        consecutive_failures = observation.consecutive_failures,
        newly_missing = observation.newly_missing,
        retry_delay_ms = poll_interval.as_millis() as u64
    );
}

fn log_storage_recovered(kind: StorageFailureKind, previous_consecutive_failures: u32) {
    tracing::info!(
        event = "ram_a.case.ingestion.resumed",
        stage = "storage",
        previous_error_kind = kind.as_str(),
        previous_consecutive_failures
    );
}

fn should_log_storage_failure(failures: u32) -> bool {
    failures <= 3 || failures.is_power_of_two()
}

async fn sleep_or_cancel(duration: Duration, cancellation_token: &CancellationToken) {
    tokio::select! {
        _ = cancellation_token.cancelled() => {}
        _ = tokio::time::sleep_until(Instant::now() + duration) => {}
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::{
        run_until_cancelled, should_log_storage_failure, StorageFailureKind,
        StorageFailureTracker,
    };
    use crate::model::{CreateDatasetRequest, CreateDocumentFileRequest};
    use crate::{build_service, CaseServiceOptions, EmbeddingProviderKind};

    fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
        let mut path = path.as_os_str().to_os_string();
        path.push(format!("-{suffix}"));
        PathBuf::from(path)
    }

    fn remove_sqlite_files(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(sqlite_sidecar_path(path, "wal"));
        let _ = std::fs::remove_file(sqlite_sidecar_path(path, "shm"));
    }

    #[test]
    fn storage_failure_logs_are_rate_limited() {
        let logged = (1..=16)
            .filter(|failures| should_log_storage_failure(*failures))
            .collect::<Vec<_>>();
        assert_eq!(logged, vec![1, 2, 3, 4, 8, 16]);
    }

    #[test]
    fn missing_database_kinds_are_counted_independently() {
        let mut tracker = StorageFailureTracker::default();

        let first_index = tracker.record(StorageFailureKind::IndexDatabaseMissing);
        assert_eq!(first_index.consecutive_failures, 1);
        assert!(first_index.newly_missing);
        assert!(first_index.should_log);

        for expected in 2..=5 {
            let repeated = tracker.record(StorageFailureKind::IndexDatabaseMissing);
            assert_eq!(repeated.consecutive_failures, expected);
            assert!(!repeated.newly_missing);
        }

        let first_business = tracker.record(StorageFailureKind::BusinessDatabaseMissing);
        assert_eq!(first_business.consecutive_failures, 1);
        assert!(first_business.newly_missing);
        assert!(first_business.should_log);

        let repeated_index = tracker.record(StorageFailureKind::IndexDatabaseMissing);
        assert_eq!(repeated_index.consecutive_failures, 6);
        assert!(!repeated_index.newly_missing);

        assert_eq!(
            tracker.recover(StorageFailureKind::BusinessDatabaseMissing),
            Some(1)
        );
        let missing_again = tracker.record(StorageFailureKind::BusinessDatabaseMissing);
        assert_eq!(missing_again.consecutive_failures, 1);
        assert!(missing_again.newly_missing);
    }

    #[test]
    fn storage_availability_reports_each_missing_database() {
        let temp = TempDir::new().unwrap();
        let rag_store = temp.path().join("cases.sqlite");
        let index_store = temp.path().join("cases-index.sqlite");
        let service = build_service(&CaseServiceOptions {
            rag_store: rag_store.clone(),
            memory_store: index_store.clone(),
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

        remove_sqlite_files(&index_store);
        let availability = service.storage_availability().unwrap();
        assert!(availability.business_database_exists);
        assert!(!availability.index_database_exists);

        remove_sqlite_files(&rag_store);
        let availability = service.storage_availability().unwrap();
        assert!(!availability.business_database_exists);
        assert!(!availability.index_database_exists);
    }

    #[tokio::test]
    async fn worker_does_not_recreate_a_missing_business_database() {
        let temp = TempDir::new().unwrap();
        let rag_store = temp.path().join("cases.sqlite");
        let service = build_service(&CaseServiceOptions {
            rag_store: rag_store.clone(),
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
        remove_sqlite_files(&rag_store);

        let cancellation = CancellationToken::new();
        let worker = tokio::spawn(run_until_cancelled(
            service,
            100,
            cancellation.clone(),
        ));
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert!(!rag_store.exists());

        cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .expect("worker should stop during missing database polling")
            .expect("worker task should not panic");
    }

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

    #[tokio::test]
    async fn idle_worker_does_not_recreate_a_deleted_index_database() {
        let temp = TempDir::new().unwrap();
        let index_store = temp.path().join("cases-index.sqlite");
        let service = build_service(&CaseServiceOptions {
            rag_store: temp.path().join("cases.sqlite"),
            memory_store: index_store.clone(),
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
        service
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
        assert!(service.run_next_ingestion_task().await.unwrap());
        remove_sqlite_files(&index_store);

        let cancellation = CancellationToken::new();
        let worker = tokio::spawn(run_until_cancelled(
            service,
            100,
            cancellation.clone(),
        ));
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert!(
            !index_store.exists(),
            "polling must not recreate a missing index database"
        );

        cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .expect("worker should stop promptly")
            .expect("worker task should not panic");
    }

    #[tokio::test]
    async fn daemon_restart_does_not_create_an_empty_index_over_existing_business_chunks() {
        let temp = TempDir::new().unwrap();
        let index_store = temp.path().join("cases-index.sqlite");
        let options = CaseServiceOptions {
            rag_store: temp.path().join("cases.sqlite"),
            memory_store: index_store.clone(),
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
        };
        let service = build_service(&options).unwrap();
        service
            .create_dataset(CreateDatasetRequest {
                id: Some("ops".to_string()),
                name: "Operations".to_string(),
                description: None,
            })
            .unwrap();
        service
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
        assert!(service.run_next_ingestion_task().await.unwrap());
        drop(service);
        remove_sqlite_files(&index_store);

        let restarted = build_service(&options).expect("restart service");
        assert!(
            !index_store.exists(),
            "startup must preserve the missing index state when business chunks exist"
        );
        let error = restarted
            .run_next_ingestion_task()
            .await
            .expect_err("worker polling should report the missing index");
        assert!(crate::is_case_index_database_missing(&error));
        assert!(!index_store.exists());
    }
}

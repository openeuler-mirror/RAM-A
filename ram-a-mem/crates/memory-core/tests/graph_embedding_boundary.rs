use std::sync::Arc;

use memory_core::{
    graph::GraphIngestionExecutor,
    sqlite::{GraphRepository, RecordEmbeddingUpdate},
    EmbeddingProvider, GraphAddMemoryRequest, MemoryError,
};

#[derive(Debug)]
struct FailingEmbedding;

#[async_trait::async_trait]
impl EmbeddingProvider for FailingEmbedding {
    fn dimensions(&self) -> usize {
        2
    }

    fn model_name(&self) -> &str {
        "failing-embedding-model"
    }

    async fn embed(&self, _texts: &[String]) -> memory_core::MemoryResult<Vec<Vec<f32>>> {
        Err(MemoryError::Embedding {
            message: "embedding unavailable".to_string(),
        })
    }
}

#[derive(Debug)]
struct FixedEmbedding;

#[async_trait::async_trait]
impl EmbeddingProvider for FixedEmbedding {
    fn dimensions(&self) -> usize {
        2
    }

    fn model_name(&self) -> &str {
        "fixed-embedding-model"
    }

    async fn embed(&self, texts: &[String]) -> memory_core::MemoryResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
}

fn request() -> GraphAddMemoryRequest {
    GraphAddMemoryRequest {
        memory_space_id: "space-a".to_string(),
        owner_id: "user-a".to_string(),
        idempotency_key: "msg-1".to_string(),
        text: "Alice lives in Shanghai.".to_string(),
        metadata: serde_json::json!({}),
        session_id: Some("session-a".to_string()),
        session_sequence: Some(1),
        source_kind: "conversation".to_string(),
        source_ref: Some("msg-1".to_string()),
        content_role: "user".to_string(),
        created_by_agent_id: None,
        observed_at_ms: None,
    }
}

#[tokio::test]
async fn vector_stage_marks_failed_without_publishing_graph() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));
    let accepted = repo.accept_memory_record(request()).await.unwrap();
    let executor = GraphIngestionExecutor::new(repo.clone(), Arc::new(FailingEmbedding));

    let error = executor
        .process_vector_stage(&accepted.ingestion_run_id)
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("embedding unavailable"));
    let run = repo
        .get_run(&accepted.ingestion_run_id, "space-a")
        .await
        .unwrap();
    assert_eq!(run.status, "failed");
    assert_eq!(run.stage, "embedding");
    assert_eq!(repo.count_facts("space-a").await.unwrap(), 0);
}

#[tokio::test]
async fn vector_stage_commits_record_embedding_before_graph_work() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));
    let accepted = repo.accept_memory_record(request()).await.unwrap();
    let executor = GraphIngestionExecutor::new(repo.clone(), Arc::new(FixedEmbedding));

    executor
        .process_vector_stage(&accepted.ingestion_run_id)
        .await
        .unwrap();

    let record = repo
        .get_graph_memory_record(&accepted.memory_record_id, "space-a")
        .await
        .unwrap();
    assert_eq!(record.embedding, Some(vec![1.0, 0.0]));
    assert_eq!(
        record.embedding_model.as_deref(),
        Some("fixed-embedding-model")
    );
    assert_eq!(
        record.embedding_version.as_deref(),
        Some("graph-embedding-v1")
    );
    let run = repo
        .get_run(&accepted.ingestion_run_id, "space-a")
        .await
        .unwrap();
    assert_eq!(run.status, "running");
    assert_eq!(run.stage, "extraction");
}

#[tokio::test]
async fn vector_stage_does_not_rewrite_embedding_after_leaving_embedding_stage() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));
    let accepted = repo.accept_memory_record(request()).await.unwrap();
    let executor = GraphIngestionExecutor::new(repo.clone(), Arc::new(FixedEmbedding));

    executor
        .process_vector_stage(&accepted.ingestion_run_id)
        .await
        .unwrap();
    let run = repo
        .get_run(&accepted.ingestion_run_id, "space-a")
        .await
        .unwrap();

    let error = repo
        .store_record_embedding(RecordEmbeddingUpdate {
            ingestion_run_id: accepted.ingestion_run_id.clone(),
            memory_record_id: accepted.memory_record_id.clone(),
            memory_space_id: "space-a".to_string(),
            attempt_count: run.attempt_count,
            embedding: vec![0.0, 1.0],
            embedding_model: "test-embedding".to_string(),
            embedding_version: "graph-embedding-v1".to_string(),
        })
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("ingestion run attempt is no longer current"));
}

#[tokio::test]
async fn vector_stage_marks_failed_when_embedding_store_fails() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("graph.sqlite");
    let repo = GraphRepository::open(&db_path);
    let accepted = repo.accept_memory_record(request()).await.unwrap();

    struct StaleRunEmbedding {
        db_path: std::path::PathBuf,
        ingestion_run_id: String,
    }

    #[async_trait::async_trait]
    impl EmbeddingProvider for StaleRunEmbedding {
        fn dimensions(&self) -> usize {
            2
        }

        fn model_name(&self) -> &str {
            "stale-run-embedding-model"
        }

        async fn embed(&self, texts: &[String]) -> memory_core::MemoryResult<Vec<Vec<f32>>> {
            let connection = rusqlite::Connection::open(&self.db_path)?;
            connection.execute_batch("PRAGMA foreign_keys = OFF;")?;
            connection.execute(
                "UPDATE graph_ingestion_runs
                 SET memory_record_id = 'stale-record-id'
                 WHERE id = ?1",
                rusqlite::params![&self.ingestion_run_id],
            )?;
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }
    }

    let executor = GraphIngestionExecutor::new(
        repo.clone(),
        Arc::new(StaleRunEmbedding {
            db_path,
            ingestion_run_id: accepted.ingestion_run_id.clone(),
        }),
    );

    let error = executor
        .process_vector_stage(&accepted.ingestion_run_id)
        .await
        .unwrap_err()
        .to_string();

    assert!(!error.is_empty());
    let run = repo
        .get_run(&accepted.ingestion_run_id, "space-a")
        .await
        .unwrap();
    assert_eq!(run.status, "failed");
    assert_eq!(run.stage, "embedding");
    assert_eq!(run.error_code.as_deref(), Some("EMBEDDING_STORE_FAILED"));
}

#[tokio::test]
async fn vector_stage_preserves_embedding_error_when_failure_mark_fails() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("graph.sqlite");
    let repo = GraphRepository::open(&db_path);
    let accepted = repo.accept_memory_record(request()).await.unwrap();

    struct StageChangingFailingEmbedding {
        db_path: std::path::PathBuf,
        ingestion_run_id: String,
    }

    #[async_trait::async_trait]
    impl EmbeddingProvider for StageChangingFailingEmbedding {
        fn dimensions(&self) -> usize {
            2
        }

        fn model_name(&self) -> &str {
            "stage-changing-failing-embedding"
        }

        async fn embed(&self, _texts: &[String]) -> memory_core::MemoryResult<Vec<Vec<f32>>> {
            let connection = rusqlite::Connection::open(&self.db_path)?;
            connection.execute(
                "UPDATE graph_ingestion_runs
                 SET stage = 'accepted'
                 WHERE id = ?1",
                rusqlite::params![&self.ingestion_run_id],
            )?;
            Err(MemoryError::Embedding {
                message: "original embedding failure".to_string(),
            })
        }
    }

    let executor = GraphIngestionExecutor::new(
        repo,
        Arc::new(StageChangingFailingEmbedding {
            db_path,
            ingestion_run_id: accepted.ingestion_run_id.clone(),
        }),
    );

    let error = executor
        .process_vector_stage(&accepted.ingestion_run_id)
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("original embedding failure"));
}

#[tokio::test]
async fn vector_stage_preserves_store_error_when_failure_mark_fails() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("graph.sqlite");
    let repo = GraphRepository::open(&db_path);
    let accepted = repo.accept_memory_record(request()).await.unwrap();

    struct StageChangingFixedEmbedding {
        db_path: std::path::PathBuf,
        ingestion_run_id: String,
    }

    #[async_trait::async_trait]
    impl EmbeddingProvider for StageChangingFixedEmbedding {
        fn dimensions(&self) -> usize {
            2
        }

        fn model_name(&self) -> &str {
            "stage-changing-fixed-embedding"
        }

        async fn embed(&self, texts: &[String]) -> memory_core::MemoryResult<Vec<Vec<f32>>> {
            let connection = rusqlite::Connection::open(&self.db_path)?;
            connection.execute(
                "UPDATE graph_ingestion_runs
                 SET stage = 'accepted'
                 WHERE id = ?1",
                rusqlite::params![&self.ingestion_run_id],
            )?;
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }
    }

    let executor = GraphIngestionExecutor::new(
        repo,
        Arc::new(StageChangingFixedEmbedding {
            db_path,
            ingestion_run_id: accepted.ingestion_run_id.clone(),
        }),
    );

    let error = executor
        .process_vector_stage(&accepted.ingestion_run_id)
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("ingestion run attempt is no longer current"));
}

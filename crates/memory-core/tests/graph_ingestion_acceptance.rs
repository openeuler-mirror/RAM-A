use memory_core::{sqlite::GraphRepository, GraphAddMemoryRequest};

fn request(idempotency_key: &str, text: &str) -> GraphAddMemoryRequest {
    GraphAddMemoryRequest {
        memory_space_id: "space-a".to_string(),
        owner_id: "user-a".to_string(),
        idempotency_key: idempotency_key.to_string(),
        text: text.to_string(),
        metadata: serde_json::json!({"source": "test"}),
        session_id: Some("session-a".to_string()),
        session_sequence: Some(1),
        source_kind: "conversation".to_string(),
        source_ref: Some("msg-1".to_string()),
        content_role: "user".to_string(),
        created_by_agent_id: None,
        observed_at_ms: None,
    }
}

fn request_for_owner(owner_id: &str, idempotency_key: &str) -> GraphAddMemoryRequest {
    GraphAddMemoryRequest {
        owner_id: owner_id.to_string(),
        idempotency_key: idempotency_key.to_string(),
        ..request(idempotency_key, "Alice lives in Shanghai.")
    }
}

#[tokio::test]
async fn accept_memory_record_is_idempotent_for_same_hash() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));

    let first = repo
        .accept_memory_record(request("msg-1", "Alice lives in Shanghai."))
        .await
        .unwrap();
    let second = repo
        .accept_memory_record(request("msg-1", "Alice lives in Shanghai."))
        .await
        .unwrap();

    assert_eq!(first.memory_record_id, second.memory_record_id);
    assert_eq!(first.ingestion_run_id, second.ingestion_run_id);
    assert_eq!(first.status, "pending");
}

#[tokio::test]
async fn accept_memory_record_rejects_same_key_with_different_hash() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));

    repo.accept_memory_record(request("msg-1", "Alice lives in Shanghai."))
        .await
        .unwrap();
    let error = repo
        .accept_memory_record(request("msg-1", "Alice lives in Hangzhou."))
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("IDEMPOTENCY_CONFLICT"));
}

#[tokio::test]
async fn accept_memory_record_rejects_existing_space_with_different_owner() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));

    repo.accept_memory_record(request_for_owner("user-a", "msg-1"))
        .await
        .unwrap();
    let error = repo
        .accept_memory_record(request_for_owner("user-b", "msg-2"))
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("MEMORY_SPACE_OWNER_MISMATCH"));
}

#[tokio::test]
async fn accept_memory_record_preserves_original_text() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("graph.sqlite");
    let repo = GraphRepository::open(&db_path);

    let accepted = repo
        .accept_memory_record(request("msg-1", "  Alice lives in Shanghai.  "))
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(db_path).unwrap();
    let text: String = connection
        .query_row(
            "SELECT text FROM graph_memory_records WHERE id = ?1",
            rusqlite::params![&accepted.memory_record_id],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(text, "  Alice lives in Shanghai.  ");
}

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use memory_core::{HashEmbedding, MemoryManager, SqliteMemoryStore};
use memory_mcp::{
    IdempotencyRepository, IngestMessage, IngestRequest, MemoryService, Principal, SearchRequest,
    ServiceError,
};
use memory_pipeline::error::Result as PipelineResult;
use memory_pipeline::extraction::{ExtractionBatch, MemoryExtractor, ModelUsage, SCHEMA_VERSION};
use memory_pipeline::grounding::{GroundingBatch, GroundingResult, GroundingVerifier};
use memory_pipeline::models::{AtomicMemory, ExtractionWindow, NormalizedMessage};
use serde_json::json;

#[derive(Default)]
struct PreferenceExtractor {
    calls: AtomicUsize,
    delay: Option<Duration>,
}

#[async_trait]
impl MemoryExtractor for PreferenceExtractor {
    fn model(&self) -> &str {
        "fixture"
    }
    fn prompt_version(&self) -> &str {
        "fixture_v1"
    }
    fn implementation(&self) -> &'static str {
        "PreferenceExtractor"
    }

    async fn extract(
        &self,
        window: &ExtractionWindow,
        messages: &HashMap<String, NormalizedMessage>,
    ) -> PipelineResult<ExtractionBatch> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        let candidate = &window.candidate_refs[0];
        let message = &messages[&candidate.message_id];
        let (memory_type, event_time) = if message.text.contains("trip") {
            (
                "event",
                json!({
                    "raw": "2026-08-01",
                    "normalized": "2026-08-01T00:00:00Z",
                    "precision": "day"
                }),
            )
        } else {
            ("preference", serde_json::Value::Null)
        };
        Ok(ExtractionBatch {
            window_id: window.id.clone(),
            schema_version: SCHEMA_VERSION.to_string(),
            raw_memories: vec![json!({
                "text": message.text,
                "memory_type": memory_type,
                "subject": {"name": "user"},
                "predicate": "prefers",
                "object": "window seat",
                "modality": "asserted",
                "event_time": event_time,
                "attributes": {},
                "evidence": [{
                    "message_id": candidate.message_id,
                    "quote": candidate.text,
                    "evidence_role": "primary"
                }]
            })],
            usage: ModelUsage::default(),
            raw_response: String::new(),
        })
    }
}

struct SupportingVerifier;

#[async_trait]
impl GroundingVerifier for SupportingVerifier {
    fn model(&self) -> &str {
        "fixture"
    }
    fn prompt_version(&self) -> &str {
        "fixture_v1"
    }
    fn implementation(&self) -> &'static str {
        "SupportingVerifier"
    }

    async fn verify(
        &self,
        window: &ExtractionWindow,
        memories: &[AtomicMemory],
        _messages: &HashMap<String, NormalizedMessage>,
    ) -> PipelineResult<GroundingBatch> {
        Ok(GroundingBatch {
            window_id: window.id.clone(),
            results: memories
                .iter()
                .map(|memory| GroundingResult {
                    memory_id: memory.id.clone(),
                    status: "SUPPORTED".to_string(),
                    reason: String::new(),
                })
                .collect(),
            usage: ModelUsage::default(),
            raw_response: String::new(),
        })
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    service: MemoryService<PreferenceExtractor, SupportingVerifier>,
    extractor: Arc<PreferenceExtractor>,
    database_path: std::path::PathBuf,
}

async fn fixture_service() -> Fixture {
    fixture_service_with_extractor(Arc::new(PreferenceExtractor::default())).await
}

async fn fixture_service_with_extractor(extractor: Arc<PreferenceExtractor>) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let database_path = temp.path().join("memory.sqlite");
    let manager = Arc::new(MemoryManager::new(
        Arc::new(SqliteMemoryStore::new(&database_path)),
        Arc::new(HashEmbedding::new(32)),
    ));
    let idempotency = IdempotencyRepository::open(&database_path).await.unwrap();
    Fixture {
        _temp: temp,
        service: MemoryService::new(
            manager,
            idempotency,
            extractor.clone(),
            Arc::new(SupportingVerifier),
        ),
        extractor,
        database_path,
    }
}

fn principal(tenant: &str, user: &str, agent: &str) -> Principal {
    Principal {
        tenant_id: tenant.to_string(),
        user_id: user.to_string(),
        agent_id: agent.to_string(),
        permissions: Vec::new(),
    }
}

fn preference_ingest() -> IngestRequest {
    IngestRequest {
        conversation_id: "conversation-1".to_string(),
        messages: vec![IngestMessage {
            id: "message-1".to_string(),
            role: "user".to_string(),
            speaker: Some("Alice".to_string()),
            text: "I prefer a window seat.".to_string(),
            timestamp: Some("2026-07-22T10:00:00Z".to_string()),
            candidate: true,
        }],
    }
}

fn search(query: &str) -> SearchRequest {
    SearchRequest {
        query: query.to_string(),
        top_k: 10,
        memory_types: Vec::new(),
        event_time_from: None,
        event_time_to: None,
    }
}

#[tokio::test]
async fn agents_for_same_user_share_but_other_users_are_isolated() {
    let fixture = fixture_service().await;
    fixture
        .service
        .ingest(&principal("t", "u", "agent-a"), preference_ingest())
        .await
        .unwrap();
    let shared = fixture
        .service
        .search(&principal("t", "u", "agent-b"), search("window seat"))
        .await
        .unwrap();
    assert_eq!(shared.memories.len(), 1);
    assert_eq!(shared.memories[0].source_agent_id, "agent-a");
    assert!(!shared.memories[0]
        .evidence_refs
        .to_string()
        .contains("message-1"));
    assert!(fixture
        .service
        .search(&principal("t", "other", "agent-c"), search("window seat"),)
        .await
        .unwrap()
        .memories
        .is_empty());
    assert!(fixture
        .service
        .search(&principal("other", "u", "agent-c"), search("window seat"),)
        .await
        .unwrap()
        .memories
        .is_empty());
}

#[tokio::test]
async fn repeated_message_id_reuses_successful_ingest() {
    let fixture = fixture_service().await;
    let principal = principal("t", "u", "agent-a");
    let first = fixture
        .service
        .ingest(&principal, preference_ingest())
        .await
        .unwrap();
    let second = fixture
        .service
        .ingest(&principal, preference_ingest())
        .await
        .unwrap();
    assert!(!first.idempotency_hit);
    assert!(second.idempotency_hit);
    assert_eq!(first.memory_ids, second.memory_ids);
}

#[tokio::test]
async fn same_message_key_with_different_content_is_rejected_before_pipeline() {
    let fixture = fixture_service().await;
    let principal = principal("tenant-secret", "user-secret", "agent-a");
    fixture
        .service
        .ingest(&principal, preference_ingest())
        .await
        .unwrap();
    let mut changed = preference_ingest();
    changed.messages[0].text = "I prefer an aisle seat.".to_string();

    let error = fixture
        .service
        .ingest(&principal, changed)
        .await
        .unwrap_err();
    assert_eq!(error, ServiceError::IdempotencyConflict);
    assert_eq!(fixture.extractor.calls.load(Ordering::SeqCst), 1);
    let rendered = format!("{error:?} {error}");
    assert!(!rendered.contains("tenant-secret"));
    assert!(!rendered.contains("user-secret"));
}

#[tokio::test]
async fn concurrent_identical_ingests_run_pipeline_once() {
    let extractor = Arc::new(PreferenceExtractor {
        calls: AtomicUsize::new(0),
        delay: Some(Duration::from_millis(50)),
    });
    let fixture = fixture_service_with_extractor(extractor.clone()).await;
    let service = Arc::new(fixture.service);
    let principal = principal("t", "u", "agent-a");

    let (first, second) = tokio::join!(
        service.ingest(&principal, preference_ingest()),
        service.ingest(&principal, preference_ingest()),
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(extractor.calls.load(Ordering::SeqCst), 1);
    assert_ne!(first.idempotency_hit, second.idempotency_hit);
    assert_eq!(first.memory_ids, second.memory_ids);
}

#[tokio::test]
async fn retry_after_memory_write_before_success_mark_upserts_stable_ids() {
    let fixture = fixture_service().await;
    let principal = principal("t", "u", "agent-a");
    let first = fixture
        .service
        .ingest(&principal, preference_ingest())
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(&fixture.database_path).unwrap();
    connection
        .execute(
            "UPDATE mcp_ingest_idempotency SET status = 'pending', result_json = NULL",
            [],
        )
        .unwrap();

    let retried = fixture
        .service
        .ingest(&principal, preference_ingest())
        .await
        .unwrap();
    let memory_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
        .unwrap();
    assert_eq!(fixture.extractor.calls.load(Ordering::SeqCst), 2);
    assert_eq!(first.memory_ids, retried.memory_ids);
    assert_eq!(memory_count, 1);
}

#[tokio::test]
async fn search_filters_type_and_event_time_after_scoped_retrieval() {
    let fixture = fixture_service().await;
    let principal = principal("t", "u", "agent-a");
    fixture
        .service
        .ingest(&principal, preference_ingest())
        .await
        .unwrap();
    let mut event = preference_ingest();
    event.conversation_id = "conversation-2".to_string();
    event.messages[0].id = "message-2".to_string();
    event.messages[0].text = "My trip starts on 2026-08-01.".to_string();
    fixture.service.ingest(&principal, event).await.unwrap();

    let request = SearchRequest {
        query: "window trip".to_string(),
        top_k: 10,
        memory_types: vec!["event".to_string()],
        event_time_from: Some("2026-07-31T00:00:00Z".to_string()),
        event_time_to: Some("2026-08-02T00:00:00Z".to_string()),
    };
    let result = fixture.service.search(&principal, request).await.unwrap();
    assert_eq!(result.memories.len(), 1);
    assert_eq!(result.memories[0].memory_type, "event");

    let excluded = SearchRequest {
        query: "window trip".to_string(),
        top_k: 10,
        memory_types: vec!["event".to_string()],
        event_time_from: Some("2026-08-02T00:00:00Z".to_string()),
        event_time_to: None,
    };
    assert!(fixture
        .service
        .search(&principal, excluded)
        .await
        .unwrap()
        .memories
        .is_empty());
}

#[tokio::test]
async fn cached_mixed_batch_reuses_all_successful_message_results() {
    let fixture = fixture_service().await;
    let principal = principal("t", "u", "agent-a");
    let first = fixture
        .service
        .ingest(&principal, preference_ingest())
        .await
        .unwrap();

    let mut combined = preference_ingest();
    combined.messages.push(IngestMessage {
        id: "message-2".to_string(),
        role: "user".to_string(),
        speaker: Some("Alice".to_string()),
        text: "I prefer quiet train cars.".to_string(),
        timestamp: Some("2026-07-22T10:01:00Z".to_string()),
        candidate: true,
    });
    let incremental = fixture
        .service
        .ingest(&principal, combined.clone())
        .await
        .unwrap();
    let repeated = fixture.service.ingest(&principal, combined).await.unwrap();

    let mut expected = first.memory_ids;
    expected.extend(incremental.memory_ids);
    expected.sort();
    expected.dedup();
    let mut actual = repeated.memory_ids;
    actual.sort();
    assert!(repeated.idempotency_hit);
    assert_eq!(fixture.extractor.calls.load(Ordering::SeqCst), 2);
    assert_eq!(actual, expected);
}

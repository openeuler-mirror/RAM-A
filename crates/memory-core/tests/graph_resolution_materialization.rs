use std::sync::Arc;

use memory_core::{
    graph::{
        ExtractedEntityCandidate, ExtractedFactCandidate, GraphEvidenceSpan,
        GraphExtractionExecutor, GraphExtractionOutput, GraphExtractor, GraphResolutionExecutor,
        GraphTypeRegistry,
    },
    sqlite::GraphRepository,
    EmbeddingProvider, GraphAddMemoryRequest,
};

#[derive(Debug)]
struct FixedEmbedding;

#[async_trait::async_trait]
impl EmbeddingProvider for FixedEmbedding {
    fn dimensions(&self) -> usize {
        2
    }

    fn model_name(&self) -> &str {
        "resolution-test-embedding"
    }

    async fn embed(&self, texts: &[String]) -> memory_core::MemoryResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
}

#[derive(Debug)]
struct FixedExtractor;

#[async_trait::async_trait]
impl GraphExtractor for FixedExtractor {
    fn extractor_name(&self) -> &str {
        "resolution-fixed-extractor"
    }

    fn model_name(&self) -> &str {
        "resolution-fixed-model"
    }

    fn prompt_version(&self) -> &str {
        "prompt-v1"
    }

    fn schema_version(&self) -> &str {
        "graph-extraction-candidates-v1"
    }

    async fn extract(
        &self,
        input: memory_core::graph::GraphExtractionInput,
    ) -> memory_core::MemoryResult<GraphExtractionOutput> {
        let evidence_text = "Alice lives in Shanghai.";
        let start = input.text.find(evidence_text).unwrap();
        let end = start + evidence_text.len();

        Ok(GraphExtractionOutput {
            entities: vec![
                ExtractedEntityCandidate {
                    local_id: "entity:alice".to_string(),
                    name: "Alice".to_string(),
                    entity_type: "PERSON".to_string(),
                    confidence: Some(0.99),
                },
                ExtractedEntityCandidate {
                    local_id: "entity:shanghai".to_string(),
                    name: "Shanghai".to_string(),
                    entity_type: "LOCATION".to_string(),
                    confidence: Some(0.98),
                },
            ],
            facts: vec![ExtractedFactCandidate {
                local_id: "fact:alice-lives-in-shanghai".to_string(),
                subject_ref: "entity:alice".to_string(),
                predicate: "LIVES_IN".to_string(),
                object_ref: "entity:shanghai".to_string(),
                fact_text: "Alice lives in Shanghai.".to_string(),
                evidence: vec![GraphEvidenceSpan {
                    text: Some(evidence_text.to_string()),
                    start_byte: Some(start),
                    end_byte: Some(end),
                }],
                confidence: Some(0.97),
                valid_from_ms: None,
                valid_to_ms: None,
            }],
            input_tokens: Some(12),
            output_tokens: Some(24),
        })
    }
}

#[derive(Debug)]
struct EmptyExtractor;

#[async_trait::async_trait]
impl GraphExtractor for EmptyExtractor {
    fn extractor_name(&self) -> &str {
        "resolution-empty-extractor"
    }

    fn model_name(&self) -> &str {
        "resolution-empty-model"
    }

    fn prompt_version(&self) -> &str {
        "prompt-v1"
    }

    fn schema_version(&self) -> &str {
        "graph-extraction-candidates-v1"
    }

    async fn extract(
        &self,
        _input: memory_core::graph::GraphExtractionInput,
    ) -> memory_core::MemoryResult<GraphExtractionOutput> {
        Ok(GraphExtractionOutput {
            entities: vec![],
            facts: vec![],
            input_tokens: Some(1),
            output_tokens: Some(1),
        })
    }
}

fn request(idempotency_key: &str, session_sequence: i64) -> GraphAddMemoryRequest {
    GraphAddMemoryRequest {
        memory_space_id: "space-a".to_string(),
        owner_id: "user-a".to_string(),
        idempotency_key: idempotency_key.to_string(),
        text: "Alice lives in Shanghai.".to_string(),
        metadata: serde_json::json!({"source": "resolution-test"}),
        session_id: Some("session-a".to_string()),
        session_sequence: Some(session_sequence),
        source_kind: "conversation".to_string(),
        source_ref: Some(idempotency_key.to_string()),
        content_role: "user".to_string(),
        created_by_agent_id: None,
        observed_at_ms: None,
    }
}

async fn run_until_resolution_ready(
    repo: &GraphRepository,
    request: GraphAddMemoryRequest,
) -> memory_core::GraphAddMemoryResponse {
    run_until_resolution_ready_with_extractor(repo, request, Arc::new(FixedExtractor)).await
}

async fn run_until_resolution_ready_with_extractor(
    repo: &GraphRepository,
    request: GraphAddMemoryRequest,
    extractor: Arc<dyn GraphExtractor>,
) -> memory_core::GraphAddMemoryResponse {
    let accepted = repo.accept_memory_record(request).await.unwrap();
    let vector_executor =
        memory_core::graph::GraphIngestionExecutor::new(repo.clone(), Arc::new(FixedEmbedding));
    vector_executor
        .process_vector_stage(&accepted.ingestion_run_id)
        .await
        .unwrap();
    let extraction_executor =
        GraphExtractionExecutor::new(repo.clone(), extractor, GraphTypeRegistry::default());
    extraction_executor
        .process_extraction_stage(&accepted.ingestion_run_id)
        .await
        .unwrap();
    accepted
}

#[tokio::test]
async fn resolution_stage_materializes_formal_graph_and_completes_run() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("graph.sqlite");
    let repo = GraphRepository::open(&db_path);
    let accepted = run_until_resolution_ready(&repo, request("msg-1", 1)).await;
    let resolution_executor =
        GraphResolutionExecutor::new(repo.clone(), GraphTypeRegistry::default());

    let result = resolution_executor
        .process_resolution_stage(&accepted.ingestion_run_id)
        .await
        .unwrap();

    assert_eq!(result.entities_created, 2);
    assert_eq!(result.entities_reused, 0);
    assert_eq!(result.facts_created, 1);
    assert_eq!(result.facts_reused, 0);
    assert_eq!(result.evidence_inserted, 1);

    let run = repo
        .get_run(&accepted.ingestion_run_id, "space-a")
        .await
        .unwrap();
    assert_eq!(run.status, "completed");
    assert_eq!(run.stage, "completed");
    assert!(run.completed_at_ms.is_some());

    let snapshot = GraphSnapshot::load(&db_path);
    assert_eq!(snapshot.entity_count, 2);
    assert_eq!(snapshot.alias_count, 2);
    assert_eq!(snapshot.fact_count, 1);
    assert_eq!(snapshot.evidence_group_count, 1);
    assert_eq!(snapshot.evidence_count, 1);
    assert_eq!(snapshot.status_history_count, 1);
    assert_eq!(snapshot.decision_count, 3);
    assert_eq!(snapshot.fact_status, "active");
    assert_eq!(snapshot.fact_text, "Alice lives in Shanghai.");
    assert_eq!(
        snapshot.evidence_text.as_deref(),
        Some("Alice lives in Shanghai.")
    );
}

#[tokio::test]
async fn resolution_stage_reuses_entities_and_facts_and_appends_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("graph.sqlite");
    let repo = GraphRepository::open(&db_path);
    let first = run_until_resolution_ready(&repo, request("msg-1", 1)).await;
    let second = run_until_resolution_ready(&repo, request("msg-2", 2)).await;
    let resolution_executor =
        GraphResolutionExecutor::new(repo.clone(), GraphTypeRegistry::default());

    resolution_executor
        .process_resolution_stage(&first.ingestion_run_id)
        .await
        .unwrap();
    let second_result = resolution_executor
        .process_resolution_stage(&second.ingestion_run_id)
        .await
        .unwrap();

    assert_eq!(second_result.entities_created, 0);
    assert_eq!(second_result.entities_reused, 2);
    assert_eq!(second_result.facts_created, 0);
    assert_eq!(second_result.facts_reused, 1);
    assert_eq!(second_result.evidence_inserted, 1);

    let snapshot = GraphSnapshot::load(&db_path);
    assert_eq!(snapshot.entity_count, 2);
    assert_eq!(snapshot.alias_count, 2);
    assert_eq!(snapshot.fact_count, 1);
    assert_eq!(snapshot.evidence_group_count, 2);
    assert_eq!(snapshot.evidence_count, 2);
    assert_eq!(snapshot.status_history_count, 1);
    assert_eq!(snapshot.decision_count, 6);
}

#[tokio::test]
async fn resolution_stage_rolls_back_partial_graph_when_publish_fails() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("graph.sqlite");
    let repo = GraphRepository::open(&db_path);
    let accepted = run_until_resolution_ready(&repo, request("msg-1", 1)).await;
    install_reject_fact_evidence_trigger(&db_path);
    let resolution_executor =
        GraphResolutionExecutor::new(repo.clone(), GraphTypeRegistry::default());

    let error = resolution_executor
        .process_resolution_stage(&accepted.ingestion_run_id)
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("reject fact evidence"));
    let run = repo
        .get_run(&accepted.ingestion_run_id, "space-a")
        .await
        .unwrap();
    assert_eq!(run.status, "failed");
    assert_eq!(run.stage, "resolving");
    assert_eq!(
        run.error_code.as_deref(),
        Some(memory_core::graph::RESOLUTION_STORE_FAILED_ERROR_CODE)
    );

    let connection = rusqlite::Connection::open(&db_path).unwrap();
    assert_eq!(count(&connection, "graph_entities"), 0);
    assert_eq!(count(&connection, "graph_entity_aliases"), 0);
    assert_eq!(count(&connection, "graph_facts"), 0);
    assert_eq!(count(&connection, "graph_fact_evidence_groups"), 0);
    assert_eq!(count(&connection, "graph_fact_evidence"), 0);
    assert_eq!(count(&connection, "graph_fact_status_history"), 0);
    assert_eq!(count(&connection, "graph_resolution_decisions"), 0);
}

#[tokio::test]
async fn resolution_stage_creates_new_entity_when_alias_match_is_ambiguous() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("graph.sqlite");
    let repo = GraphRepository::open(&db_path);
    let accepted = run_until_resolution_ready(&repo, request("msg-1", 1)).await;
    install_ambiguous_alice_aliases(&db_path);
    let resolution_executor =
        GraphResolutionExecutor::new(repo.clone(), GraphTypeRegistry::default());

    let result = resolution_executor
        .process_resolution_stage(&accepted.ingestion_run_id)
        .await
        .unwrap();

    assert_eq!(result.entities_created, 2);
    assert_eq!(result.entities_reused, 0);
    assert_eq!(result.facts_created, 1);

    let connection = rusqlite::Connection::open(&db_path).unwrap();
    assert_eq!(count(&connection, "graph_entities"), 4);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*)
                 FROM graph_entities
                 WHERE memory_space_id = 'space-a'
                   AND entity_type = 'PERSON'
                   AND normalized_name = 'alice'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn resolution_stage_completes_empty_candidate_output_without_graph_rows() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("graph.sqlite");
    let repo = GraphRepository::open(&db_path);
    let accepted = run_until_resolution_ready_with_extractor(
        &repo,
        request("msg-1", 1),
        Arc::new(EmptyExtractor),
    )
    .await;
    let resolution_executor =
        GraphResolutionExecutor::new(repo.clone(), GraphTypeRegistry::default());

    let result = resolution_executor
        .process_resolution_stage(&accepted.ingestion_run_id)
        .await
        .unwrap();

    assert_eq!(result.entities_created, 0);
    assert_eq!(result.entities_reused, 0);
    assert_eq!(result.facts_created, 0);
    assert_eq!(result.facts_reused, 0);
    assert_eq!(result.evidence_inserted, 0);
    assert_eq!(result.ingestion_run.status, "completed");
    assert_eq!(result.ingestion_run.stage, "completed");

    let connection = rusqlite::Connection::open(&db_path).unwrap();
    assert_eq!(count(&connection, "graph_entities"), 0);
    assert_eq!(count(&connection, "graph_facts"), 0);
    assert_eq!(count(&connection, "graph_fact_evidence_groups"), 0);
    assert_eq!(count(&connection, "graph_fact_evidence"), 0);
    assert_eq!(count(&connection, "graph_resolution_decisions"), 0);
}

struct GraphSnapshot {
    entity_count: i64,
    alias_count: i64,
    fact_count: i64,
    evidence_group_count: i64,
    evidence_count: i64,
    status_history_count: i64,
    decision_count: i64,
    fact_status: String,
    fact_text: String,
    evidence_text: Option<String>,
}

impl GraphSnapshot {
    fn load(db_path: &std::path::Path) -> Self {
        let connection = rusqlite::Connection::open(db_path).unwrap();
        Self {
            entity_count: count(&connection, "graph_entities"),
            alias_count: count(&connection, "graph_entity_aliases"),
            fact_count: count(&connection, "graph_facts"),
            evidence_group_count: count(&connection, "graph_fact_evidence_groups"),
            evidence_count: count(&connection, "graph_fact_evidence"),
            status_history_count: count(&connection, "graph_fact_status_history"),
            decision_count: count(&connection, "graph_resolution_decisions"),
            fact_status: connection
                .query_row("SELECT status FROM graph_facts", [], |row| row.get(0))
                .unwrap(),
            fact_text: connection
                .query_row("SELECT fact_text FROM graph_facts", [], |row| row.get(0))
                .unwrap(),
            evidence_text: connection
                .query_row(
                    "SELECT evidence_text FROM graph_fact_evidence LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap(),
        }
    }
}

fn count(connection: &rusqlite::Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

// This failure injection intentionally targets the current evidence INSERT path.
// If publish stops inserting graph_fact_evidence directly, update the injection point.
fn install_reject_fact_evidence_trigger(db_path: &std::path::Path) {
    let connection = rusqlite::Connection::open(db_path).unwrap();
    connection
        .execute_batch(
            r#"
            CREATE TRIGGER reject_fact_evidence
            BEFORE INSERT ON graph_fact_evidence
            BEGIN
                SELECT RAISE(ABORT, 'reject fact evidence');
            END;
            "#,
        )
        .unwrap();
}

fn install_ambiguous_alice_aliases(db_path: &std::path::Path) {
    let connection = rusqlite::Connection::open(db_path).unwrap();
    connection
        .execute_batch(
            r#"
            INSERT INTO graph_entities (
                id, memory_space_id, canonical_name, normalized_name, entity_type,
                status, type_registry_version, created_at_ms, updated_at_ms
            ) VALUES
                ('entity-alice-smith', 'space-a', 'Alice Smith', 'alice smith', 'PERSON',
                 'active', 'graph-type-registry-v1', 1, 1),
                ('entity-alice-wong', 'space-a', 'Alice Wong', 'alice wong', 'PERSON',
                 'active', 'graph-type-registry-v1', 1, 1);

            INSERT INTO graph_entity_aliases (
                id, memory_space_id, entity_id, display_alias, normalized_alias, created_at_ms
            ) VALUES
                ('alias-alice-smith', 'space-a', 'entity-alice-smith', 'Alice', 'alice', 1),
                ('alias-alice-wong', 'space-a', 'entity-alice-wong', 'Alice', 'alice', 1);
            "#,
        )
        .unwrap();
}

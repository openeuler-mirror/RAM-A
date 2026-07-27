use std::sync::Arc;
use std::sync::Mutex;

use memory_core::MemoryError;
use memory_core::{
    graph::{
        ExtractedEntityCandidate, ExtractedFactCandidate, GraphEvidenceSpan,
        GraphExtractionExecutor, GraphExtractionOutput, GraphExtractor, GraphResolutionExecutor,
        GraphTypeRegistry,
    },
    sqlite::GraphRepository,
    AddMemoryRequest, EmbeddingProvider, GraphAddMemoryRequest, GraphRetrievalConfig,
    GraphRetrieveContextRequest, LongTermMemory, MemoryManager, MemoryResult, RerankConfig,
    Reranker, RetrievalConfig, ScoredMemory, SearchMemoryRequest, SearchMode, SqliteMemoryStore,
};

#[derive(Debug)]
struct FixedEmbedding;

#[async_trait::async_trait]
impl EmbeddingProvider for FixedEmbedding {
    fn dimensions(&self) -> usize {
        2
    }

    fn model_name(&self) -> &str {
        "retrieval-test-embedding"
    }

    async fn embed(&self, texts: &[String]) -> memory_core::MemoryResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
}

#[derive(Debug)]
struct RecordingReranker {
    seen_texts: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait::async_trait]
impl Reranker for RecordingReranker {
    async fn rerank(
        &self,
        _query: &str,
        mut candidates: Vec<ScoredMemory>,
        top_k: usize,
    ) -> MemoryResult<Vec<ScoredMemory>> {
        self.seen_texts.lock().expect("seen texts lock").push(
            candidates
                .iter()
                .map(|candidate| candidate.record.text.clone())
                .collect(),
        );
        candidates.truncate(top_k);
        Ok(candidates)
    }
}

#[derive(Debug)]
struct SingleFactExtractor;

#[async_trait::async_trait]
impl GraphExtractor for SingleFactExtractor {
    fn extractor_name(&self) -> &str {
        "retrieval-single-fact-extractor"
    }

    fn model_name(&self) -> &str {
        "retrieval-fixed-model"
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
                temporal_expression: None,
                valid_from_ms: Some(1_650_000_000_000),
                valid_to_ms: None,
            }],
            input_tokens: Some(12),
            output_tokens: Some(24),
        })
    }
}

#[derive(Debug)]
struct TwoFactExtractor;

#[async_trait::async_trait]
impl GraphExtractor for TwoFactExtractor {
    fn extractor_name(&self) -> &str {
        "retrieval-two-fact-extractor"
    }

    fn model_name(&self) -> &str {
        "retrieval-fixed-model"
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
        let shanghai_evidence = "Alice lives in Shanghai.";
        let shanghai_start = input.text.find(shanghai_evidence).unwrap();
        let shanghai_end = shanghai_start + shanghai_evidence.len();
        let beijing_evidence = "Alice lives in Beijing.";
        let beijing_start = input.text.find(beijing_evidence).unwrap();
        let beijing_end = beijing_start + beijing_evidence.len();

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
                ExtractedEntityCandidate {
                    local_id: "entity:beijing".to_string(),
                    name: "Beijing".to_string(),
                    entity_type: "LOCATION".to_string(),
                    confidence: Some(0.98),
                },
            ],
            facts: vec![
                ExtractedFactCandidate {
                    local_id: "fact:alice-lives-in-shanghai".to_string(),
                    subject_ref: "entity:alice".to_string(),
                    predicate: "LIVES_IN".to_string(),
                    object_ref: "entity:shanghai".to_string(),
                    fact_text: "Alice lives in Shanghai.".to_string(),
                    evidence: vec![GraphEvidenceSpan {
                        text: Some(shanghai_evidence.to_string()),
                        start_byte: Some(shanghai_start),
                        end_byte: Some(shanghai_end),
                    }],
                    confidence: Some(0.97),
                    temporal_expression: None,
                    valid_from_ms: None,
                    valid_to_ms: None,
                },
                ExtractedFactCandidate {
                    local_id: "fact:alice-lives-in-beijing".to_string(),
                    subject_ref: "entity:alice".to_string(),
                    predicate: "LIVES_IN".to_string(),
                    object_ref: "entity:beijing".to_string(),
                    fact_text: "Alice lives in Beijing.".to_string(),
                    evidence: vec![GraphEvidenceSpan {
                        text: Some(beijing_evidence.to_string()),
                        start_byte: Some(beijing_start),
                        end_byte: Some(beijing_end),
                    }],
                    confidence: Some(0.96),
                    temporal_expression: None,
                    valid_from_ms: None,
                    valid_to_ms: None,
                },
            ],
            input_tokens: Some(20),
            output_tokens: Some(40),
        })
    }
}

#[derive(Debug)]
struct EmptyExtractor;

#[async_trait::async_trait]
impl GraphExtractor for EmptyExtractor {
    fn extractor_name(&self) -> &str {
        "retrieval-empty-extractor"
    }

    fn model_name(&self) -> &str {
        "retrieval-fixed-model"
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
            entities: Vec::new(),
            facts: Vec::new(),
            input_tokens: Some(4),
            output_tokens: Some(2),
        })
    }
}

fn request(
    memory_space_id: &str,
    owner_id: &str,
    idempotency_key: &str,
    session_sequence: i64,
    text: &str,
) -> GraphAddMemoryRequest {
    request_with_metadata(
        memory_space_id,
        owner_id,
        idempotency_key,
        session_sequence,
        text,
        serde_json::json!({"source": "retrieval-test"}),
    )
}

fn request_with_metadata(
    memory_space_id: &str,
    owner_id: &str,
    idempotency_key: &str,
    session_sequence: i64,
    text: &str,
    metadata: serde_json::Value,
) -> GraphAddMemoryRequest {
    GraphAddMemoryRequest {
        memory_space_id: memory_space_id.to_string(),
        owner_id: owner_id.to_string(),
        idempotency_key: idempotency_key.to_string(),
        text: text.to_string(),
        metadata,
        session_id: Some("session-a".to_string()),
        session_sequence: Some(session_sequence),
        source_kind: "conversation".to_string(),
        source_ref: Some(idempotency_key.to_string()),
        content_role: "user".to_string(),
        created_by_agent_id: None,
        observed_at_ms: None,
    }
}

async fn run_completed_graph_fixture(repo: &GraphRepository) {
    run_completed_single_fact(repo, "msg-1", 1).await;
}

async fn run_completed_single_fact(repo: &GraphRepository, idempotency_key: &str, sequence: i64) {
    run_completed_with_extractor(
        repo,
        request(
            "space-1",
            "user-1",
            idempotency_key,
            sequence,
            "Alice lives in Shanghai.",
        ),
        Arc::new(SingleFactExtractor),
    )
    .await;
}

async fn run_completed_with_extractor(
    repo: &GraphRepository,
    request: GraphAddMemoryRequest,
    extractor: Arc<dyn GraphExtractor>,
) {
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
    let resolution_executor =
        GraphResolutionExecutor::new(repo.clone(), GraphTypeRegistry::default());
    resolution_executor
        .process_resolution_stage(&accepted.ingestion_run_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn graph_retrieval_finds_fact_from_entity_seed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));

    run_completed_graph_fixture(&repo).await;

    let bundle = repo
        .retrieve_context(GraphRetrieveContextRequest {
            memory_space_id: "space-1".to_string(),
            query: "Alice".to_string(),
            top_k: 5,
            reference_time_ms: Some(1_700_000_000_000),
            query_embedding: None,
            query_embedding_model: None,
            target_subject_entity_name: None,
            target_evidence_speaker: None,
            seed_limit: Some(10),
            max_evidence_records_per_fact: Some(2),
        })
        .await
        .expect("retrieve graph context");

    assert_eq!(bundle.memory_space_id, "space-1");
    assert_eq!(bundle.query, "Alice");
    assert_eq!(bundle.reference_time_ms, 1_700_000_000_000);
    assert_eq!(bundle.fact_context_units.len(), 1);
    assert_eq!(
        bundle.fact_context_units[0].fact_text,
        "Alice lives in Shanghai."
    );
    assert_eq!(
        bundle.fact_context_units[0].subject_entity.canonical_name,
        "Alice"
    );
    assert_eq!(
        bundle.fact_context_units[0].object_entity.canonical_name,
        "Shanghai"
    );
    assert_eq!(bundle.fact_context_units[0].evidence_records.len(), 1);
    assert_eq!(bundle.evidence_record_context_units.len(), 1);
    assert!(
        bundle.fact_context_units[0].score > bundle.evidence_record_context_units[0].score,
        "a matching fact must outrank its raw evidence-node fallback"
    );
    assert_eq!(
        bundle.fact_context_units[0].valid_from_ms,
        Some(1_650_000_000_000)
    );
    assert_eq!(bundle.fact_context_units[0].valid_to_ms, None);
    assert_eq!(
        bundle.fact_context_units[0].recorded_at_ms,
        bundle.facts[0].recorded_at_ms
    );
    assert!(bundle.fact_context_units[0]
        .path
        .iter()
        .any(|step| step.starts_with("fact:")));
}

#[tokio::test]
async fn graph_retrieval_returns_direct_evidence_for_a_record_without_facts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));
    run_completed_with_extractor(
        &repo,
        request_with_metadata(
            "space-1",
            "user-1",
            "raw-adoption",
            1,
            "Alice is researching adoption agencies.",
            serde_json::json!({"speaker": "Alice"}),
        ),
        Arc::new(EmptyExtractor),
    )
    .await;

    let bundle = repo
        .retrieve_context(GraphRetrieveContextRequest {
            memory_space_id: "space-1".to_string(),
            query: "adoption research".to_string(),
            top_k: 5,
            reference_time_ms: None,
            query_embedding: None,
            query_embedding_model: None,
            target_subject_entity_name: None,
            target_evidence_speaker: Some("Alice".to_string()),
            seed_limit: Some(10),
            max_evidence_records_per_fact: Some(2),
        })
        .await
        .expect("retrieve direct graph evidence");

    assert!(bundle.fact_context_units.is_empty());
    assert_eq!(bundle.evidence_record_context_units.len(), 1);
    let unit = &bundle.evidence_record_context_units[0];
    assert_eq!(unit.record.source_ref.as_deref(), Some("raw-adoption"));
    assert!(unit.path.iter().any(|step| step.starts_with("record:")));
    assert!(unit.score > 0.0);
    assert_eq!(bundle.records.len(), 1);
}

#[tokio::test]
async fn graph_retrieval_lexical_evidence_does_not_decode_record_embedding() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("graph.sqlite");
    let repo = GraphRepository::open(&db_path);
    run_completed_with_extractor(
        &repo,
        request_with_metadata(
            "space-1",
            "user-1",
            "raw-adoption",
            1,
            "Alice is researching adoption agencies.",
            serde_json::json!({"speaker": "Alice"}),
        ),
        Arc::new(EmptyExtractor),
    )
    .await;

    let connection = rusqlite::Connection::open(&db_path).expect("open graph database");
    connection
        .execute(
            "UPDATE graph_memory_records
             SET embedding = ?1
             WHERE source_ref = ?2",
            rusqlite::params![vec![1_u8, 2, 3], "raw-adoption"],
        )
        .expect("corrupt embedding blob");

    let bundle = repo
        .retrieve_context(GraphRetrieveContextRequest {
            memory_space_id: "space-1".to_string(),
            query: "adoption research".to_string(),
            top_k: 5,
            reference_time_ms: None,
            query_embedding: None,
            query_embedding_model: None,
            target_subject_entity_name: None,
            target_evidence_speaker: Some("Alice".to_string()),
            seed_limit: Some(10),
            max_evidence_records_per_fact: Some(2),
        })
        .await
        .expect("retrieve lexical graph evidence");

    assert_eq!(bundle.evidence_record_context_units.len(), 1);
    assert_eq!(
        bundle.evidence_record_context_units[0]
            .record
            .source_ref
            .as_deref(),
        Some("raw-adoption")
    );
}

#[tokio::test]
async fn graph_retrieval_filters_direct_evidence_by_target_speaker() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));
    for (id, speaker) in [("alice-adoption", "Alice"), ("bob-adoption", "Bob")] {
        run_completed_with_extractor(
            &repo,
            request_with_metadata(
                "space-1",
                "user-1",
                id,
                1,
                "Adoption agencies are being discussed.",
                serde_json::json!({"speaker": speaker}),
            ),
            Arc::new(EmptyExtractor),
        )
        .await;
    }

    let bundle = repo
        .retrieve_context(GraphRetrieveContextRequest {
            memory_space_id: "space-1".to_string(),
            query: "adoption agencies".to_string(),
            top_k: 5,
            reference_time_ms: None,
            query_embedding: None,
            query_embedding_model: None,
            target_subject_entity_name: None,
            target_evidence_speaker: Some("Alice".to_string()),
            seed_limit: Some(10),
            max_evidence_records_per_fact: Some(2),
        })
        .await
        .expect("retrieve speaker-filtered direct graph evidence");

    assert_eq!(bundle.evidence_record_context_units.len(), 1);
    assert_eq!(
        bundle.evidence_record_context_units[0].record.metadata["speaker"],
        "Alice"
    );
}

#[tokio::test]
async fn graph_retrieval_finds_entities_and_evidence_from_fact_seed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));

    run_completed_graph_fixture(&repo).await;

    let bundle = repo
        .retrieve_context(GraphRetrieveContextRequest {
            memory_space_id: "space-1".to_string(),
            query: "lives Shanghai".to_string(),
            top_k: 5,
            reference_time_ms: Some(1_700_000_000_000),
            query_embedding: None,
            query_embedding_model: None,
            target_subject_entity_name: None,
            target_evidence_speaker: None,
            seed_limit: Some(10),
            max_evidence_records_per_fact: Some(2),
        })
        .await
        .expect("retrieve graph context");

    assert_eq!(bundle.fact_context_units.len(), 1);
    let unit = &bundle.fact_context_units[0];
    assert_eq!(unit.subject_entity.canonical_name, "Alice");
    assert_eq!(unit.object_entity.canonical_name, "Shanghai");
    assert_eq!(unit.evidence_records[0].text, "Alice lives in Shanghai.");
    assert!(unit.path[0].starts_with("fact:"));
    assert!(bundle
        .records
        .iter()
        .any(|record| record.text == "Alice lives in Shanghai."));
    assert!(bundle
        .entities
        .iter()
        .any(|entity| entity.canonical_name == "Alice"));
    assert!(bundle
        .facts
        .iter()
        .any(|fact| fact.fact_text == "Alice lives in Shanghai."));
}

#[tokio::test]
async fn graph_retrieval_does_not_cross_memory_space() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));

    run_completed_graph_fixture(&repo).await;

    let bundle = repo
        .retrieve_context(GraphRetrieveContextRequest {
            memory_space_id: "space-2".to_string(),
            query: "Alice".to_string(),
            top_k: 5,
            reference_time_ms: Some(1_700_000_000_000),
            query_embedding: None,
            query_embedding_model: None,
            target_subject_entity_name: None,
            target_evidence_speaker: None,
            seed_limit: Some(10),
            max_evidence_records_per_fact: Some(2),
        })
        .await
        .expect("retrieve graph context");

    assert_eq!(bundle.memory_space_id, "space-2");
    assert!(bundle.fact_context_units.is_empty());
    assert!(bundle.records.is_empty());
    assert!(bundle.entities.is_empty());
    assert!(bundle.facts.is_empty());
}

#[tokio::test]
async fn graph_retrieval_returns_empty_bundle_for_no_match() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));

    run_completed_graph_fixture(&repo).await;

    let bundle = repo
        .retrieve_context(GraphRetrieveContextRequest {
            memory_space_id: "space-1".to_string(),
            query: "unmatched-term".to_string(),
            top_k: 5,
            reference_time_ms: Some(1_700_000_000_000),
            query_embedding: None,
            query_embedding_model: None,
            target_subject_entity_name: None,
            target_evidence_speaker: None,
            seed_limit: Some(10),
            max_evidence_records_per_fact: Some(2),
        })
        .await
        .expect("retrieve graph context");

    assert!(bundle.fact_context_units.is_empty());
    assert!(bundle.records.is_empty());
    assert_eq!(bundle.degraded_reason, None);
}

#[tokio::test]
async fn graph_retrieval_applies_top_k_and_evidence_limits() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));

    run_completed_with_extractor(
        &repo,
        request(
            "space-1",
            "user-1",
            "msg-1",
            1,
            "Alice lives in Shanghai. Alice lives in Beijing.",
        ),
        Arc::new(TwoFactExtractor),
    )
    .await;
    run_completed_single_fact(&repo, "msg-2", 2).await;

    let one_fact_bundle = repo
        .retrieve_context(GraphRetrieveContextRequest {
            memory_space_id: "space-1".to_string(),
            query: "Alice".to_string(),
            top_k: 1,
            reference_time_ms: Some(1_700_000_000_000),
            query_embedding: None,
            query_embedding_model: None,
            target_subject_entity_name: None,
            target_evidence_speaker: None,
            seed_limit: Some(10),
            max_evidence_records_per_fact: Some(2),
        })
        .await
        .expect("retrieve graph context");
    assert_eq!(one_fact_bundle.fact_context_units.len(), 1);

    let evidence_limited_bundle = repo
        .retrieve_context(GraphRetrieveContextRequest {
            memory_space_id: "space-1".to_string(),
            query: "Shanghai".to_string(),
            top_k: 5,
            reference_time_ms: Some(1_700_000_000_000),
            query_embedding: None,
            query_embedding_model: None,
            target_subject_entity_name: None,
            target_evidence_speaker: None,
            seed_limit: Some(10),
            max_evidence_records_per_fact: Some(1),
        })
        .await
        .expect("retrieve graph context");
    let shanghai_unit = evidence_limited_bundle
        .fact_context_units
        .iter()
        .find(|unit| unit.fact_text == "Alice lives in Shanghai.")
        .expect("Shanghai fact context unit");
    assert_eq!(shanghai_unit.evidence_records.len(), 1);
}

#[tokio::test]
async fn graph_retrieval_filters_evidence_records_by_target_speaker() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));

    run_completed_with_extractor(
        &repo,
        request_with_metadata(
            "space-1",
            "user-1",
            "msg-alice",
            1,
            "Alice lives in Shanghai.",
            serde_json::json!({"source": "retrieval-test", "speaker": "Alice"}),
        ),
        Arc::new(SingleFactExtractor),
    )
    .await;
    run_completed_with_extractor(
        &repo,
        request_with_metadata(
            "space-1",
            "user-1",
            "msg-bob",
            2,
            "Alice lives in Shanghai.",
            serde_json::json!({"source": "retrieval-test", "speaker": "Bob"}),
        ),
        Arc::new(SingleFactExtractor),
    )
    .await;

    let bundle = repo
        .retrieve_context(GraphRetrieveContextRequest {
            memory_space_id: "space-1".to_string(),
            query: "Alice Shanghai".to_string(),
            top_k: 5,
            reference_time_ms: Some(1_700_000_000_000),
            query_embedding: None,
            query_embedding_model: None,
            target_subject_entity_name: Some("Alice".to_string()),
            target_evidence_speaker: Some("Alice".to_string()),
            seed_limit: Some(10),
            max_evidence_records_per_fact: Some(5),
        })
        .await
        .expect("retrieve graph context");

    assert_eq!(bundle.fact_context_units.len(), 1);
    let speakers = bundle.fact_context_units[0]
        .evidence_records
        .iter()
        .map(|record| record.metadata["speaker"].as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(speakers, vec!["Alice"]);
}

#[tokio::test]
async fn memory_manager_search_can_include_graph_channel_when_enabled() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("memory.sqlite");
    let repo = GraphRepository::open(&db_path);
    run_completed_graph_fixture(&repo).await;

    let store = Arc::new(SqliteMemoryStore::new(&db_path));
    let manager = MemoryManager::with_retrieval_config(
        store,
        Arc::new(FixedEmbedding),
        RetrievalConfig {
            mode: SearchMode::Bm25,
            graph: GraphRetrievalConfig {
                enabled: true,
                weight: 1.0,
                rerank_with_graph: true,
                allow_graph_only: true,
                fail_open: false,
                seed_limit: Some(10),
                max_evidence_records_per_fact: Some(2),
            },
            ..RetrievalConfig::default()
        },
    );

    let results = manager
        .search(SearchMemoryRequest {
            query: "Alice".to_string(),
            top_k: 5,
            filter: None,
            graph_memory_space_id: Some("space-1".to_string()),
            graph_target_subject: None,
            graph_target_evidence_speaker: None,
        })
        .await
        .expect("search with graph channel");

    assert!(results
        .iter()
        .any(|result| result.record.text == "Alice lives in Shanghai."));
    let graph_result = results
        .iter()
        .find(|result| result.record.text == "Alice lives in Shanghai.")
        .expect("graph-backed result");
    assert_eq!(
        graph_result.record.metadata["graph_facts"][0]["fact_text"],
        "Alice lives in Shanghai."
    );
    assert_eq!(
        graph_result.record.metadata["graph_facts"][0]["predicate"],
        "LIVES_IN"
    );
    assert_eq!(
        graph_result.record.metadata["graph_facts"][0]["subject"]["name"],
        "Alice"
    );
    assert_eq!(
        graph_result.record.metadata["graph_facts"][0]["object"]["name"],
        "Shanghai"
    );
    assert_eq!(
        graph_result.record.metadata["graph_facts"][0]["valid_from_ms"],
        1_650_000_000_000_u64
    );
    assert!(graph_result.record.metadata["graph_facts"][0]["valid_to_ms"].is_null());
    assert!(
        graph_result.record.metadata["graph_facts"][0]["recorded_at_ms"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
}

#[tokio::test]
async fn memory_manager_graph_search_returns_only_graph_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("memory.sqlite");
    let repo = GraphRepository::open(&db_path);
    run_completed_graph_fixture(&repo).await;

    let store = Arc::new(SqliteMemoryStore::new(&db_path));
    let manager = MemoryManager::with_retrieval_config(
        store,
        Arc::new(FixedEmbedding),
        RetrievalConfig {
            mode: SearchMode::Graph,
            graph: GraphRetrievalConfig {
                enabled: true,
                fail_open: false,
                seed_limit: Some(10),
                max_evidence_records_per_fact: Some(2),
                ..GraphRetrievalConfig::default()
            },
            ..RetrievalConfig::default()
        },
    );

    manager
        .add(AddMemoryRequest {
            id: Some("raw-only".to_string()),
            text: "Shanghai is a raw-memory distractor.".to_string(),
            metadata: serde_json::json!({"source": "base-only"}),
        })
        .await
        .expect("seed base-only memory");

    let results = manager
        .search(SearchMemoryRequest {
            query: "Shanghai".to_string(),
            top_k: 5,
            filter: None,
            graph_memory_space_id: Some("space-1".to_string()),
            graph_target_subject: None,
            graph_target_evidence_speaker: None,
        })
        .await
        .expect("graph-only search");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].record.id, "msg-1");
    assert_eq!(results[0].record.text, "Alice lives in Shanghai.");
    assert!(results.iter().all(|result| result.record.id != "raw-only"));
    assert_eq!(
        results[0].record.metadata["graph_facts"][0]["fact_text"],
        "Alice lives in Shanghai."
    );
}

#[tokio::test]
async fn memory_manager_graph_search_returns_direct_evidence_without_creating_a_fact() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("memory.sqlite");
    let repo = GraphRepository::open(&db_path);
    run_completed_with_extractor(
        &repo,
        request_with_metadata(
            "space-1",
            "user-1",
            "raw-adoption",
            1,
            "Alice is researching adoption agencies.",
            serde_json::json!({"speaker": "Alice"}),
        ),
        Arc::new(EmptyExtractor),
    )
    .await;

    let manager = MemoryManager::with_retrieval_config(
        Arc::new(SqliteMemoryStore::new(&db_path)),
        Arc::new(FixedEmbedding),
        RetrievalConfig {
            mode: SearchMode::Graph,
            graph: GraphRetrievalConfig {
                enabled: true,
                fail_open: false,
                seed_limit: Some(10),
                ..GraphRetrievalConfig::default()
            },
            ..RetrievalConfig::default()
        },
    );

    let results = manager
        .search(SearchMemoryRequest {
            query: "adoption research".to_string(),
            top_k: 5,
            filter: None,
            graph_memory_space_id: Some("space-1".to_string()),
            graph_target_subject: None,
            graph_target_evidence_speaker: Some("Alice".to_string()),
        })
        .await
        .expect("graph-only direct evidence search");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].record.id, "raw-adoption");
    assert_eq!(
        results[0].record.metadata["graph_matches"][0]["kind"],
        "evidence_record"
    );
    assert!(results[0].record.metadata.get("graph_facts").is_none());
}

#[tokio::test]
async fn memory_manager_graph_channel_merges_with_base_record_by_source_ref() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("memory.sqlite");
    let repo = GraphRepository::open(&db_path);
    run_completed_graph_fixture(&repo).await;

    let store = Arc::new(SqliteMemoryStore::new(&db_path));
    let manager = MemoryManager::with_retrieval_config(
        store,
        Arc::new(FixedEmbedding),
        RetrievalConfig {
            mode: SearchMode::Bm25,
            graph: GraphRetrievalConfig {
                enabled: true,
                weight: 1.0,
                rerank_with_graph: false,
                allow_graph_only: false,
                fail_open: false,
                seed_limit: Some(10),
                max_evidence_records_per_fact: Some(2),
            },
            ..RetrievalConfig::default()
        },
    );
    manager
        .add(AddMemoryRequest {
            id: Some("msg-1".to_string()),
            text: "Alice lives in Shanghai.".to_string(),
            metadata: serde_json::json!({"source": "retrieval-test"}),
        })
        .await
        .expect("seed base memory");

    let results = manager
        .search(SearchMemoryRequest {
            query: "Alice".to_string(),
            top_k: 5,
            filter: None,
            graph_memory_space_id: Some("space-1".to_string()),
            graph_target_subject: None,
            graph_target_evidence_speaker: None,
        })
        .await
        .expect("search with graph channel");

    let matching_records = results
        .iter()
        .filter(|result| result.record.text == "Alice lives in Shanghai.")
        .collect::<Vec<_>>();
    assert_eq!(matching_records.len(), 1, "results: {results:?}");
    assert_eq!(matching_records[0].record.id, "msg-1");
    assert_eq!(
        matching_records[0].record.metadata["graph_facts"][0]["fact_text"],
        "Alice lives in Shanghai."
    );
}

#[tokio::test]
async fn memory_manager_graph_channel_requires_memory_space_when_fail_closed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(SqliteMemoryStore::new(temp.path().join("memory.sqlite")));
    let manager = MemoryManager::with_retrieval_config(
        store,
        Arc::new(FixedEmbedding),
        RetrievalConfig {
            mode: SearchMode::Bm25,
            graph: GraphRetrievalConfig {
                enabled: true,
                fail_open: false,
                ..GraphRetrievalConfig::default()
            },
            ..RetrievalConfig::default()
        },
    );

    let error = manager
        .search(SearchMemoryRequest {
            query: "Alice".to_string(),
            top_k: 5,
            filter: None,
            graph_memory_space_id: None,
            graph_target_subject: None,
            graph_target_evidence_speaker: None,
        })
        .await
        .expect_err("missing graph memory space should fail closed");

    assert!(format!("{error}").contains("graph_memory_space_id is missing"));
}

#[tokio::test]
async fn memory_manager_graph_channel_can_fail_open_when_space_is_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(SqliteMemoryStore::new(temp.path().join("memory.sqlite")));
    let manager = MemoryManager::with_retrieval_config(
        store,
        Arc::new(FixedEmbedding),
        RetrievalConfig {
            mode: SearchMode::Bm25,
            graph: GraphRetrievalConfig {
                enabled: true,
                fail_open: true,
                ..GraphRetrievalConfig::default()
            },
            ..RetrievalConfig::default()
        },
    );

    let results = manager
        .search(SearchMemoryRequest {
            query: "Alice".to_string(),
            top_k: 5,
            filter: None,
            graph_memory_space_id: None,
            graph_target_subject: None,
            graph_target_evidence_speaker: None,
        })
        .await
        .expect("missing graph memory space should fail open");

    assert!(results.is_empty());
}

#[tokio::test]
async fn memory_manager_graph_channel_fails_closed_on_graph_repository_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("memory.sqlite");
    install_broken_graph_entity_fts_table(&db_path);
    let store = Arc::new(SqliteMemoryStore::new(&db_path));
    let manager = MemoryManager::with_retrieval_config(
        store,
        Arc::new(FixedEmbedding),
        RetrievalConfig {
            mode: SearchMode::Dense,
            graph: GraphRetrievalConfig {
                enabled: true,
                fail_open: false,
                ..GraphRetrievalConfig::default()
            },
            ..RetrievalConfig::default()
        },
    );

    let error = manager
        .search(SearchMemoryRequest {
            query: "Alice".to_string(),
            top_k: 5,
            filter: None,
            graph_memory_space_id: Some("space-1".to_string()),
            graph_target_subject: None,
            graph_target_evidence_speaker: None,
        })
        .await
        .expect_err("graph repository error should fail closed");

    assert!(
        matches!(error, MemoryError::Sqlite(_)),
        "expected sqlite graph repository error, got {error:?}"
    );
}

#[tokio::test]
async fn memory_manager_graph_channel_fails_open_on_graph_repository_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("memory.sqlite");
    install_broken_graph_entity_fts_table(&db_path);
    let store = Arc::new(SqliteMemoryStore::new(&db_path));
    let manager = MemoryManager::with_retrieval_config(
        store,
        Arc::new(FixedEmbedding),
        RetrievalConfig {
            mode: SearchMode::Dense,
            graph: GraphRetrievalConfig {
                enabled: true,
                fail_open: true,
                ..GraphRetrievalConfig::default()
            },
            ..RetrievalConfig::default()
        },
    );

    let results = manager
        .search(SearchMemoryRequest {
            query: "Alice".to_string(),
            top_k: 5,
            filter: None,
            graph_memory_space_id: Some("space-1".to_string()),
            graph_target_subject: None,
            graph_target_evidence_speaker: None,
        })
        .await
        .expect("graph repository error should fail open");

    assert!(results.is_empty());
}

#[tokio::test]
async fn memory_manager_graph_channel_fails_closed_for_non_sqlite_store() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(memory_core::FileMemoryStore::new(
        temp.path().join("memory.jsonl"),
    ));
    let manager = MemoryManager::with_retrieval_config(
        store,
        Arc::new(FixedEmbedding),
        RetrievalConfig {
            mode: SearchMode::Dense,
            graph: GraphRetrievalConfig {
                enabled: true,
                fail_open: false,
                ..GraphRetrievalConfig::default()
            },
            ..RetrievalConfig::default()
        },
    );

    let error = manager
        .search(SearchMemoryRequest {
            query: "Alice".to_string(),
            top_k: 5,
            filter: None,
            graph_memory_space_id: Some("space-1".to_string()),
            graph_target_subject: None,
            graph_target_evidence_speaker: None,
        })
        .await
        .expect_err("non-sqlite graph channel should fail closed");

    assert!(format!("{error}").contains("graph retrieval requires sqlite store backend"));
}

#[tokio::test]
async fn memory_manager_graph_channel_fails_open_for_non_sqlite_store() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(memory_core::FileMemoryStore::new(
        temp.path().join("memory.jsonl"),
    ));
    let manager = MemoryManager::with_retrieval_config(
        store,
        Arc::new(FixedEmbedding),
        RetrievalConfig {
            mode: SearchMode::Dense,
            graph: GraphRetrievalConfig {
                enabled: true,
                fail_open: true,
                ..GraphRetrievalConfig::default()
            },
            ..RetrievalConfig::default()
        },
    );

    let results = manager
        .search(SearchMemoryRequest {
            query: "Alice".to_string(),
            top_k: 5,
            filter: None,
            graph_memory_space_id: Some("space-1".to_string()),
            graph_target_subject: None,
            graph_target_evidence_speaker: None,
        })
        .await
        .expect("non-sqlite graph channel should fail open");

    assert!(results.is_empty());
}

#[tokio::test]
async fn memory_manager_rerank_receives_graph_candidates() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("memory.sqlite");
    let repo = GraphRepository::open(&db_path);
    run_completed_graph_fixture(&repo).await;

    let seen_texts = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(SqliteMemoryStore::new(&db_path));
    let manager = MemoryManager::with_retrieval_config_and_reranker(
        store,
        Arc::new(FixedEmbedding),
        RetrievalConfig {
            mode: SearchMode::Hybrid,
            graph: GraphRetrievalConfig {
                enabled: true,
                rerank_with_graph: true,
                allow_graph_only: true,
                seed_limit: Some(10),
                max_evidence_records_per_fact: Some(2),
                ..GraphRetrievalConfig::default()
            },
            rerank: RerankConfig {
                enabled: true,
                input_k: 5,
                fail_open: false,
                ..RerankConfig::default()
            },
            ..RetrievalConfig::default()
        },
        Arc::new(RecordingReranker {
            seen_texts: seen_texts.clone(),
        }),
    );

    manager
        .search(SearchMemoryRequest {
            query: "Alice".to_string(),
            top_k: 1,
            filter: None,
            graph_memory_space_id: Some("space-1".to_string()),
            graph_target_subject: None,
            graph_target_evidence_speaker: None,
        })
        .await
        .expect("graph candidates should enter rerank");

    let seen = seen_texts.lock().expect("seen texts lock");
    assert_eq!(seen.len(), 1);
    assert!(seen[0].contains(&"Alice lives in Shanghai.".to_string()));
}

fn install_broken_graph_entity_fts_table(db_path: &std::path::Path) {
    let connection = rusqlite::Connection::open(db_path).expect("open sqlite");
    connection
        .execute("CREATE TABLE graph_entity_fts (unexpected TEXT)", [])
        .expect("install broken graph_entity_fts table");
}

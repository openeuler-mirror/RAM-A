use std::sync::Arc;

use memory_core::graph::{
    ExtractedEntityCandidate, ExtractedFactCandidate, GraphBuildPipeline, GraphEvidenceSpan,
    GraphExtractionInput, GraphExtractionOutput, GraphExtractor, GraphTypeRegistry,
};
use memory_core::sqlite::GraphRepository;
use memory_core::{
    GraphAddMemoryRequest, GraphRetrieveContextRequest, HashEmbedding, MemoryError, MemoryResult,
};

#[derive(Debug)]
struct LivesInExtractor;

#[derive(Debug)]
struct FailingExtractor;

#[async_trait::async_trait]
impl GraphExtractor for FailingExtractor {
    fn extractor_name(&self) -> &str {
        "failing-test-extractor"
    }

    fn model_name(&self) -> &str {
        "test-model"
    }

    fn prompt_version(&self) -> &str {
        "test-prompt-v1"
    }

    fn schema_version(&self) -> &str {
        "test-schema-v1"
    }

    async fn extract(&self, _input: GraphExtractionInput) -> MemoryResult<GraphExtractionOutput> {
        Err(MemoryError::Extraction {
            message: "transient extraction failure".to_string(),
        })
    }
}

#[async_trait::async_trait]
impl GraphExtractor for LivesInExtractor {
    fn extractor_name(&self) -> &str {
        "test-extractor"
    }

    fn model_name(&self) -> &str {
        "test-model"
    }

    fn prompt_version(&self) -> &str {
        "test-prompt-v1"
    }

    fn schema_version(&self) -> &str {
        "test-schema-v1"
    }

    async fn extract(&self, input: GraphExtractionInput) -> MemoryResult<GraphExtractionOutput> {
        Ok(GraphExtractionOutput {
            entities: vec![
                ExtractedEntityCandidate {
                    local_id: "entity:alice".to_string(),
                    name: "Alice".to_string(),
                    entity_type: "PERSON".to_string(),
                    confidence: Some(1.0),
                },
                ExtractedEntityCandidate {
                    local_id: "entity:shanghai".to_string(),
                    name: "Shanghai".to_string(),
                    entity_type: "LOCATION".to_string(),
                    confidence: Some(1.0),
                },
            ],
            facts: vec![ExtractedFactCandidate {
                local_id: "fact:alice-shanghai".to_string(),
                subject_ref: "entity:alice".to_string(),
                predicate: "LIVES_IN".to_string(),
                object_ref: "entity:shanghai".to_string(),
                fact_text: "Alice lives in Shanghai.".to_string(),
                evidence: vec![GraphEvidenceSpan {
                    text: Some(input.text),
                    start_byte: None,
                    end_byte: None,
                }],
                confidence: Some(1.0),
                temporal_expression: None,
                valid_from_ms: None,
                valid_to_ms: None,
            }],
            input_tokens: Some(10),
            output_tokens: Some(20),
        })
    }
}

#[tokio::test]
async fn graph_build_pipeline_materializes_queryable_graph() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("graph.sqlite");
    let repository = GraphRepository::open(&db_path);
    let registry = GraphTypeRegistry::new_default();
    let pipeline = GraphBuildPipeline::new(
        repository.clone(),
        Arc::new(HashEmbedding::new(8)),
        Arc::new(LivesInExtractor),
        registry,
    );

    let result = pipeline
        .build_memory(GraphAddMemoryRequest {
            memory_space_id: "scope-a".to_string(),
            owner_id: "benchmark".to_string(),
            idempotency_key: "record-1".to_string(),
            text: "Alice lives in Shanghai.".to_string(),
            metadata: serde_json::json!({
                "scope_id": "scope-a",
                "path": "$[0].conversation.session_1[0].text"
            }),
            session_id: None,
            session_sequence: None,
            source_kind: "benchmark".to_string(),
            source_ref: Some("record-1".to_string()),
            content_role: "message".to_string(),
            created_by_agent_id: None,
            observed_at_ms: None,
        })
        .await
        .expect("build graph memory");

    assert_eq!(result.add_response.status, "pending");
    assert_eq!(result.resolution.entities_created, 2);
    assert_eq!(result.resolution.facts_created, 1);

    let bundle = repository
        .retrieve_context(GraphRetrieveContextRequest {
            memory_space_id: "scope-a".to_string(),
            query: "Where does Alice live?".to_string(),
            top_k: 5,
            reference_time_ms: None,
            query_embedding: None,
            query_embedding_model: None,
            target_subject_entity_name: None,
            target_evidence_speaker: None,
            seed_limit: None,
            max_evidence_records_per_fact: None,
        })
        .await
        .expect("retrieve context");

    assert_eq!(bundle.fact_context_units.len(), 1);
    assert_eq!(
        bundle.fact_context_units[0].evidence_records[0].text,
        "Alice lives in Shanghai."
    );
}

#[tokio::test]
async fn graph_build_pipeline_skips_completed_idempotent_memory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("graph.sqlite");
    let repository = GraphRepository::open(&db_path);
    let registry = GraphTypeRegistry::new_default();
    let pipeline = GraphBuildPipeline::new(
        repository.clone(),
        Arc::new(HashEmbedding::new(8)),
        Arc::new(LivesInExtractor),
        registry,
    );

    let request = GraphAddMemoryRequest {
        memory_space_id: "scope-a".to_string(),
        owner_id: "benchmark".to_string(),
        idempotency_key: "record-1".to_string(),
        text: "Alice lives in Shanghai.".to_string(),
        metadata: serde_json::json!({
            "scope_id": "scope-a",
            "path": "$[0].conversation.session_1[0].text"
        }),
        session_id: None,
        session_sequence: None,
        source_kind: "benchmark".to_string(),
        source_ref: Some("record-1".to_string()),
        content_role: "message".to_string(),
        created_by_agent_id: None,
        observed_at_ms: None,
    };

    pipeline
        .build_memory(request.clone())
        .await
        .expect("first graph build");
    assert_eq!(repository.count_facts("scope-a").await.unwrap(), 1);

    let repeated = pipeline
        .build_memory_if_needed(request)
        .await
        .expect("repeated graph build should be idempotent");

    assert!(repeated.is_none());
    assert_eq!(repository.count_facts("scope-a").await.unwrap(), 1);
}

#[tokio::test]
async fn graph_build_pipeline_resumes_failed_extraction_with_a_new_attempt() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repository = GraphRepository::open(temp.path().join("graph.sqlite"));
    let registry = GraphTypeRegistry::new_default();
    let request = GraphAddMemoryRequest {
        memory_space_id: "scope-a".to_string(),
        owner_id: "benchmark".to_string(),
        idempotency_key: "record-1".to_string(),
        text: "Alice lives in Shanghai.".to_string(),
        metadata: serde_json::json!({"scope_id": "scope-a"}),
        session_id: None,
        session_sequence: None,
        source_kind: "benchmark".to_string(),
        source_ref: Some("record-1".to_string()),
        content_role: "message".to_string(),
        created_by_agent_id: None,
        observed_at_ms: None,
    };
    let failing_pipeline = GraphBuildPipeline::new(
        repository.clone(),
        Arc::new(HashEmbedding::new(8)),
        Arc::new(FailingExtractor),
        registry.clone(),
    );

    failing_pipeline
        .build_memory(request.clone())
        .await
        .expect_err("first extraction must fail");
    let failed = repository
        .accept_memory_record(request.clone())
        .await
        .unwrap();
    assert_eq!(failed.status, "failed");

    let resumed_pipeline = GraphBuildPipeline::new(
        repository.clone(),
        Arc::new(HashEmbedding::new(8)),
        Arc::new(LivesInExtractor),
        registry,
    );
    let resumed = resumed_pipeline
        .resume_memory(request)
        .await
        .expect("resume failed graph build")
        .expect("failed graph build should be retried");

    assert_eq!(resumed.ingestion_run.status, "completed");
    assert_eq!(resumed.ingestion_run.attempt_count, 2);
    assert_eq!(resumed.extraction_run.attempt_number, 2);
    assert_eq!(repository.count_facts("scope-a").await.unwrap(), 1);
}

#[tokio::test]
async fn graph_build_pipeline_resumes_interrupted_running_attempt() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repository = GraphRepository::open(temp.path().join("graph.sqlite"));
    let registry = GraphTypeRegistry::new_default();
    let request = GraphAddMemoryRequest {
        memory_space_id: "scope-a".to_string(),
        owner_id: "benchmark".to_string(),
        idempotency_key: "record-1".to_string(),
        text: "Alice lives in Shanghai.".to_string(),
        metadata: serde_json::json!({"scope_id": "scope-a"}),
        session_id: None,
        session_sequence: None,
        source_kind: "benchmark".to_string(),
        source_ref: Some("record-1".to_string()),
        content_role: "message".to_string(),
        created_by_agent_id: None,
        observed_at_ms: None,
    };
    let accepted = repository
        .accept_memory_record(request.clone())
        .await
        .unwrap();
    repository
        .claim_pending_run(&accepted.ingestion_run_id)
        .await
        .expect("simulate interrupted running attempt");
    assert_eq!(
        repository
            .get_run(&accepted.ingestion_run_id, "scope-a")
            .await
            .unwrap()
            .status,
        "running"
    );

    let pipeline = GraphBuildPipeline::new(
        repository.clone(),
        Arc::new(HashEmbedding::new(8)),
        Arc::new(LivesInExtractor),
        registry,
    );
    let resumed = pipeline
        .resume_memory(request)
        .await
        .expect("resume interrupted graph build")
        .expect("running graph build should be retried");

    assert_eq!(resumed.ingestion_run.status, "completed");
    assert_eq!(resumed.ingestion_run.attempt_count, 2);
    assert_eq!(repository.count_facts("scope-a").await.unwrap(), 1);
}

use std::sync::Arc;

use memory_core::{
    graph::{
        ExtractedEntityCandidate, ExtractedFactCandidate, GraphEvidenceSpan,
        GraphExtractionExecutor, GraphExtractionOutput, GraphExtractor, GraphTypeRegistry,
    },
    sqlite::GraphRepository,
    EmbeddingProvider, GraphAddMemoryRequest, MemoryError,
};

#[derive(Debug)]
struct FixedEmbedding;

#[async_trait::async_trait]
impl EmbeddingProvider for FixedEmbedding {
    fn dimensions(&self) -> usize {
        2
    }

    fn model_name(&self) -> &str {
        "extraction-test-embedding"
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
        "fixed-extractor"
    }

    fn model_name(&self) -> &str {
        "fixed-extraction-model"
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
                valid_from_ms: None,
                valid_to_ms: None,
            }],
            input_tokens: Some(12),
            output_tokens: Some(24),
        })
    }
}

#[derive(Debug)]
struct FailingExtractor;

#[async_trait::async_trait]
impl GraphExtractor for FailingExtractor {
    fn extractor_name(&self) -> &str {
        "failing-extractor"
    }

    fn model_name(&self) -> &str {
        "failing-extraction-model"
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
        Err(MemoryError::StoreBackend {
            message: "extractor unavailable".to_string(),
        })
    }
}

#[derive(Debug)]
struct InvalidExtractor;

#[async_trait::async_trait]
impl GraphExtractor for InvalidExtractor {
    fn extractor_name(&self) -> &str {
        "invalid-extractor"
    }

    fn model_name(&self) -> &str {
        "invalid-extraction-model"
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
            entities: vec![ExtractedEntityCandidate {
                local_id: "entity:alice".to_string(),
                name: "Alice".to_string(),
                entity_type: "PERSON".to_string(),
                confidence: Some(0.99),
            }],
            facts: vec![ExtractedFactCandidate {
                local_id: "fact:invalid".to_string(),
                subject_ref: "entity:alice".to_string(),
                predicate: "LIVES_IN".to_string(),
                object_ref: "entity:missing-location".to_string(),
                fact_text: "Alice lives in Shanghai.".to_string(),
                evidence: vec![],
                confidence: Some(0.97),
                temporal_expression: None,
                valid_from_ms: None,
                valid_to_ms: None,
            }],
            input_tokens: None,
            output_tokens: None,
        })
    }
}

fn request() -> GraphAddMemoryRequest {
    GraphAddMemoryRequest {
        memory_space_id: "space-a".to_string(),
        owner_id: "user-a".to_string(),
        idempotency_key: "msg-1".to_string(),
        text: "Alice lives in Shanghai.".to_string(),
        metadata: serde_json::json!({"source": "extraction-test"}),
        session_id: Some("session-a".to_string()),
        session_sequence: Some(1),
        source_kind: "conversation".to_string(),
        source_ref: Some("msg-1".to_string()),
        content_role: "user".to_string(),
        created_by_agent_id: None,
        observed_at_ms: None,
    }
}

async fn prepare_record_for_extraction(
    repo: &GraphRepository,
) -> memory_core::GraphAddMemoryResponse {
    let accepted = repo.accept_memory_record(request()).await.unwrap();
    let vector_executor =
        memory_core::graph::GraphIngestionExecutor::new(repo.clone(), Arc::new(FixedEmbedding));
    vector_executor
        .process_vector_stage(&accepted.ingestion_run_id)
        .await
        .unwrap();
    accepted
}

fn fetch_failed_extraction_status(
    db_path: &std::path::Path,
    ingestion_run_id: &str,
) -> (String, Option<String>, Option<String>) {
    let connection = rusqlite::Connection::open(db_path).unwrap();
    connection
        .query_row(
            "SELECT status, error_code, error_message
             FROM graph_extraction_runs
             WHERE ingestion_run_id = ?1",
            rusqlite::params![ingestion_run_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .unwrap()
}

// This failure injection intentionally targets the current completed extraction-run INSERT path.
// If success persistence stops inserting graph_extraction_runs directly, update the injection point.
fn install_reject_completed_extraction_trigger(db_path: &std::path::Path) {
    let connection = rusqlite::Connection::open(db_path).unwrap();
    connection
        .execute_batch(
            r#"
            CREATE TRIGGER reject_completed_extraction
            BEFORE INSERT ON graph_extraction_runs
            WHEN NEW.status = 'completed'
            BEGIN
                SELECT RAISE(ABORT, 'reject completed extraction');
            END;
            "#,
        )
        .unwrap();
}

fn valid_output_with_evidence(evidence: Vec<GraphEvidenceSpan>) -> GraphExtractionOutput {
    GraphExtractionOutput {
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
            evidence,
            confidence: Some(0.97),
            temporal_expression: None,
            valid_from_ms: None,
            valid_to_ms: None,
        }],
        input_tokens: None,
        output_tokens: None,
    }
}

fn assert_invalid_output(output: GraphExtractionOutput, record_text: &str, expected: &str) {
    let error = output
        .validate_against_record(record_text, &GraphTypeRegistry::default())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(expected),
        "expected error to contain '{expected}', got '{error}'"
    );
}

#[tokio::test]
async fn extraction_stage_persists_structured_candidates_and_advances_run() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));
    let accepted = prepare_record_for_extraction(&repo).await;

    let extraction_executor = GraphExtractionExecutor::new(
        repo.clone(),
        Arc::new(FixedExtractor),
        GraphTypeRegistry::default(),
    );

    let extraction_run = extraction_executor
        .process_extraction_stage(&accepted.ingestion_run_id)
        .await
        .unwrap();

    assert_eq!(extraction_run.status, "completed");
    assert_eq!(extraction_run.ingestion_run_id, accepted.ingestion_run_id);
    assert_eq!(
        extraction_run.context_record_ids,
        vec![accepted.memory_record_id]
    );

    let stored_extraction = repo
        .get_extraction_run(&extraction_run.id, "space-a")
        .await
        .unwrap();
    assert_eq!(stored_extraction.status, "completed");

    let output: GraphExtractionOutput =
        serde_json::from_value(stored_extraction.structured_output.unwrap()).unwrap();
    assert_eq!(output.entities.len(), 2);
    assert_eq!(output.facts.len(), 1);
    assert_eq!(output.facts[0].predicate, "LIVES_IN");

    let run = repo
        .get_run(&accepted.ingestion_run_id, "space-a")
        .await
        .unwrap();
    assert_eq!(run.status, "running");
    assert_eq!(run.stage, "resolution");
}

#[tokio::test]
async fn graph_repository_getters_are_scoped_by_memory_space() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));
    let accepted = prepare_record_for_extraction(&repo).await;

    let extraction_executor = GraphExtractionExecutor::new(
        repo.clone(),
        Arc::new(FixedExtractor),
        GraphTypeRegistry::default(),
    );
    let extraction_run = extraction_executor
        .process_extraction_stage(&accepted.ingestion_run_id)
        .await
        .unwrap();

    assert!(repo
        .get_run(&accepted.ingestion_run_id, "wrong-space")
        .await
        .is_err());
    assert!(repo
        .get_graph_memory_record(&accepted.memory_record_id, "wrong-space")
        .await
        .is_err());
    assert!(repo
        .get_extraction_run(&extraction_run.id, "wrong-space")
        .await
        .is_err());
}

#[tokio::test]
async fn extraction_stage_marks_failed_when_success_store_fails() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("graph.sqlite");
    let repo = GraphRepository::open(&db_path);
    let accepted = prepare_record_for_extraction(&repo).await;
    install_reject_completed_extraction_trigger(&db_path);

    let extraction_executor = GraphExtractionExecutor::new(
        repo.clone(),
        Arc::new(FixedExtractor),
        GraphTypeRegistry::default(),
    );

    let error = extraction_executor
        .process_extraction_stage(&accepted.ingestion_run_id)
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("reject completed extraction"));
    let run = repo
        .get_run(&accepted.ingestion_run_id, "space-a")
        .await
        .unwrap();
    assert_eq!(run.status, "failed");
    assert_eq!(run.stage, "extracting");
    assert_eq!(run.error_code.as_deref(), Some("EXTRACTION_STORE_FAILED"));

    let (status, error_code, error_message) =
        fetch_failed_extraction_status(&db_path, &accepted.ingestion_run_id);
    assert_eq!(status, "failed");
    assert_eq!(error_code.as_deref(), Some("EXTRACTION_STORE_FAILED"));
    assert!(error_message
        .unwrap()
        .contains("reject completed extraction"));
}

#[tokio::test]
async fn extraction_stage_records_failed_run_when_extractor_fails() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("graph.sqlite");
    let repo = GraphRepository::open(&db_path);
    let accepted = prepare_record_for_extraction(&repo).await;
    let extraction_executor = GraphExtractionExecutor::new(
        repo.clone(),
        Arc::new(FailingExtractor),
        GraphTypeRegistry::default(),
    );

    let error = extraction_executor
        .process_extraction_stage(&accepted.ingestion_run_id)
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("extractor unavailable"));
    let run = repo
        .get_run(&accepted.ingestion_run_id, "space-a")
        .await
        .unwrap();
    assert_eq!(run.status, "failed");
    assert_eq!(run.stage, "extracting");
    assert_eq!(run.error_code.as_deref(), Some("EXTRACTION_FAILED"));

    let (status, error_code, error_message) =
        fetch_failed_extraction_status(&db_path, &accepted.ingestion_run_id);
    assert_eq!(status, "failed");
    assert_eq!(error_code.as_deref(), Some("EXTRACTION_FAILED"));
    assert!(error_message.unwrap().contains("extractor unavailable"));
}

#[tokio::test]
async fn extraction_stage_rejects_invalid_structured_candidates() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("graph.sqlite");
    let repo = GraphRepository::open(&db_path);
    let accepted = prepare_record_for_extraction(&repo).await;
    let extraction_executor = GraphExtractionExecutor::new(
        repo.clone(),
        Arc::new(InvalidExtractor),
        GraphTypeRegistry::default(),
    );

    let error = extraction_executor
        .process_extraction_stage(&accepted.ingestion_run_id)
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("INVALID_EXTRACTION_OUTPUT"));
    assert!(error.contains("unknown object"));
    let run = repo
        .get_run(&accepted.ingestion_run_id, "space-a")
        .await
        .unwrap();
    assert_eq!(run.status, "failed");
    assert_eq!(run.stage, "extracting");
    assert_eq!(run.error_code.as_deref(), Some("INVALID_EXTRACTION_OUTPUT"));

    let (status, error_code, error_message) =
        fetch_failed_extraction_status(&db_path, &accepted.ingestion_run_id);
    assert_eq!(status, "failed");
    assert_eq!(error_code.as_deref(), Some("INVALID_EXTRACTION_OUTPUT"));
    assert!(error_message.unwrap().contains("unknown object"));
}

#[test]
fn extraction_validation_rejects_ungrounded_evidence() {
    let record_text = "Alice lives in Shanghai.";
    let registry = GraphTypeRegistry::default();

    let no_evidence = valid_output_with_evidence(vec![])
        .validate_against_record(record_text, &registry)
        .unwrap_err()
        .to_string();
    assert!(no_evidence.contains("must include at least one evidence"));

    let empty_evidence = valid_output_with_evidence(vec![GraphEvidenceSpan {
        text: None,
        start_byte: None,
        end_byte: None,
    }])
    .validate_against_record(record_text, &registry)
    .unwrap_err()
    .to_string();
    assert!(empty_evidence.contains("must include text or byte range"));

    let missing_text_evidence = valid_output_with_evidence(vec![GraphEvidenceSpan {
        text: Some("Alice works in Berlin.".to_string()),
        start_byte: None,
        end_byte: None,
    }])
    .validate_against_record(record_text, &registry)
    .unwrap_err()
    .to_string();
    assert!(missing_text_evidence.contains("evidence text is not present in record text"));
}

#[test]
fn extraction_validation_rejects_invalid_candidate_structure() {
    let record_text = "Alice lives in Shanghai.";
    let grounded_evidence = vec![GraphEvidenceSpan {
        text: Some(record_text.to_string()),
        start_byte: Some(0),
        end_byte: Some(record_text.len()),
    }];

    let mut duplicate_entity = valid_output_with_evidence(grounded_evidence.clone());
    duplicate_entity.entities[1].local_id = duplicate_entity.entities[0].local_id.clone();
    assert_invalid_output(duplicate_entity, record_text, "duplicate entity local_id");

    let mut unknown_entity_type = valid_output_with_evidence(grounded_evidence.clone());
    unknown_entity_type.entities[0].entity_type = "UNKNOWN".to_string();
    assert_invalid_output(unknown_entity_type, record_text, "unknown entity type");

    let mut duplicate_fact = valid_output_with_evidence(grounded_evidence.clone());
    duplicate_fact.facts.push(duplicate_fact.facts[0].clone());
    assert_invalid_output(duplicate_fact, record_text, "duplicate fact local_id");

    let mut unknown_predicate = valid_output_with_evidence(grounded_evidence.clone());
    unknown_predicate.facts[0].predicate = "UNKNOWN_PREDICATE".to_string();
    assert_invalid_output(unknown_predicate, record_text, "unknown predicate");

    let mut invalid_validity = valid_output_with_evidence(grounded_evidence.clone());
    invalid_validity.facts[0].valid_from_ms = Some(200);
    invalid_validity.facts[0].valid_to_ms = Some(100);
    assert_invalid_output(
        invalid_validity,
        record_text,
        "valid_from_ms after valid_to_ms",
    );

    let mut invalid_entity_confidence = valid_output_with_evidence(grounded_evidence.clone());
    invalid_entity_confidence.entities[0].confidence = Some(1.1);
    assert_invalid_output(
        invalid_entity_confidence,
        record_text,
        "entity.confidence must be between 0 and 1",
    );

    let mut invalid_fact_confidence = valid_output_with_evidence(grounded_evidence);
    invalid_fact_confidence.facts[0].confidence = Some(f32::NAN);
    assert_invalid_output(
        invalid_fact_confidence,
        record_text,
        "fact.confidence must be between 0 and 1",
    );
}

#[test]
fn extraction_validation_rejects_invalid_evidence_ranges() {
    let record_text = "Alice lives in Shanghai.";

    assert_invalid_output(
        valid_output_with_evidence(vec![GraphEvidenceSpan {
            text: None,
            start_byte: Some(0),
            end_byte: Some(0),
        }]),
        record_text,
        "outside record text",
    );

    assert_invalid_output(
        valid_output_with_evidence(vec![GraphEvidenceSpan {
            text: None,
            start_byte: Some(0),
            end_byte: Some(record_text.len() + 1),
        }]),
        record_text,
        "outside record text",
    );

    assert_invalid_output(
        valid_output_with_evidence(vec![GraphEvidenceSpan {
            text: None,
            start_byte: Some(0),
            end_byte: None,
        }]),
        record_text,
        "must be set together",
    );

    assert_invalid_output(
        valid_output_with_evidence(vec![GraphEvidenceSpan {
            text: Some("Bob".to_string()),
            start_byte: Some(0),
            end_byte: Some(5),
        }]),
        record_text,
        "does not match record byte range",
    );

    let unicode_text = "Alice lives in 上海.";
    let chinese_start = unicode_text.find("上海").unwrap();
    assert_invalid_output(
        valid_output_with_evidence(vec![GraphEvidenceSpan {
            text: None,
            start_byte: Some(chinese_start),
            end_byte: Some(chinese_start + 1),
        }]),
        unicode_text,
        "not on UTF-8 boundaries",
    );
}

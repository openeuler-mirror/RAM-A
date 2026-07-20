use std::sync::{Arc, Mutex};

use memory_core::{
    graph::{
        GraphExtractionExecutor, GraphExtractor, GraphLlmClient, GraphLlmRequest, GraphLlmResponse,
        GraphTypeRegistry, LlmGraphExtractor,
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
        "llm-extraction-test-embedding"
    }

    async fn embed(&self, texts: &[String]) -> memory_core::MemoryResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
}

#[derive(Debug)]
struct FakeGraphLlmClient {
    response: String,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    requests: Mutex<Vec<GraphLlmRequest>>,
}

impl FakeGraphLlmClient {
    fn new(response: impl Into<String>) -> Self {
        Self {
            response: response.into(),
            input_tokens: Some(17),
            output_tokens: Some(29),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<GraphLlmRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl GraphLlmClient for FakeGraphLlmClient {
    fn client_name(&self) -> &str {
        "fake-llm-client"
    }

    fn model_name(&self) -> &str {
        "fake-llm-model"
    }

    async fn complete_json(
        &self,
        request: GraphLlmRequest,
    ) -> memory_core::MemoryResult<GraphLlmResponse> {
        self.requests.lock().unwrap().push(request);
        Ok(GraphLlmResponse {
            content: self.response.clone(),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
        })
    }
}

fn request() -> GraphAddMemoryRequest {
    GraphAddMemoryRequest {
        memory_space_id: "space-a".to_string(),
        owner_id: "user-a".to_string(),
        idempotency_key: "msg-1".to_string(),
        text: "Alice lives in Shanghai.".to_string(),
        metadata: serde_json::json!({"source": "llm-extraction-test"}),
        session_id: Some("session-a".to_string()),
        session_sequence: Some(1),
        source_kind: "conversation".to_string(),
        source_ref: Some("msg-1".to_string()),
        content_role: "user".to_string(),
        created_by_agent_id: None,
        observed_at_ms: None,
    }
}

fn valid_llm_json() -> String {
    serde_json::json!({
        "entities": [
            {
                "local_id": "entity:alice",
                "name": "Alice",
                "entity_type": "PERSON",
                "confidence": 0.99
            },
            {
                "local_id": "entity:shanghai",
                "name": "Shanghai",
                "entity_type": "LOCATION",
                "confidence": 0.98
            }
        ],
        "facts": [
            {
                "local_id": "fact:alice-lives-in-shanghai",
                "subject_ref": "entity:alice",
                "predicate": "LIVES_IN",
                "object_ref": "entity:shanghai",
                "fact_text": "Alice lives in Shanghai.",
                "evidence": [
                    {
                        "text": "Alice lives in Shanghai.",
                        "start_byte": 0,
                        "end_byte": 24
                    }
                ],
                "confidence": 0.97,
                "valid_from_ms": null,
                "valid_to_ms": null
            }
        ]
    })
    .to_string()
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

#[tokio::test]
async fn llm_graph_extractor_persists_fake_llm_candidates_and_usage() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));
    let accepted = prepare_record_for_extraction(&repo).await;
    let fake_client = Arc::new(FakeGraphLlmClient::new(valid_llm_json()));
    let extractor = LlmGraphExtractor::new(fake_client.clone(), GraphTypeRegistry::default());
    let executor = GraphExtractionExecutor::new(
        repo.clone(),
        Arc::new(extractor),
        GraphTypeRegistry::default(),
    );

    let extraction_run = executor
        .process_extraction_stage(&accepted.ingestion_run_id)
        .await
        .unwrap();

    let stored = repo
        .get_extraction_run(&extraction_run.id, "space-a")
        .await
        .unwrap();
    assert_eq!(stored.status, "completed");
    assert_eq!(stored.extractor_name, "llm-graph-extractor");
    assert_eq!(stored.model, "fake-llm-model");
    assert_eq!(stored.prompt_version, "graph-extraction-prompt-v1");
    assert_eq!(stored.schema_version, "graph-extraction-candidates-v1");
    assert_eq!(stored.input_tokens, Some(17));
    assert_eq!(stored.output_tokens, Some(29));

    let output = stored.structured_output.unwrap();
    assert_eq!(output["entities"].as_array().unwrap().len(), 2);
    assert_eq!(output["facts"].as_array().unwrap().len(), 1);

    let requests = fake_client.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].response_format_json);
    assert_eq!(requests[0].temperature, 0.0);
    let user_prompt = requests[0]
        .messages
        .iter()
        .find(|message| message.role == "user")
        .unwrap()
        .content
        .as_str();
    assert!(user_prompt.contains("Alice lives in Shanghai."));
    assert!(user_prompt.contains("start_byte"));
}

#[tokio::test]
async fn llm_graph_extractor_accepts_fenced_json_response() {
    let fenced = format!("```json\n{}\n```", valid_llm_json());
    let extractor = LlmGraphExtractor::new(
        Arc::new(FakeGraphLlmClient::new(fenced)),
        GraphTypeRegistry::default(),
    );

    let output = extractor
        .extract(memory_core::graph::GraphExtractionInput {
            memory_space_id: "space-a".to_string(),
            memory_record_id: "record-a".to_string(),
            text: "Alice lives in Shanghai.".to_string(),
            metadata: serde_json::json!({}),
            context_record_ids: vec!["record-a".to_string()],
            type_registry_version: "graph-type-registry-v1".to_string(),
        })
        .await
        .unwrap();

    assert_eq!(output.entities.len(), 2);
    assert_eq!(output.facts.len(), 1);
}

#[tokio::test]
async fn llm_graph_extractor_repairs_evidence_byte_offsets_from_exact_text() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));
    let accepted = prepare_record_for_extraction(&repo).await;
    let invalid_offsets_json = serde_json::json!({
        "entities": [
            {
                "local_id": "entity:alice",
                "name": "Alice",
                "entity_type": "PERSON",
                "confidence": 0.99
            },
            {
                "local_id": "entity:shanghai",
                "name": "Shanghai",
                "entity_type": "LOCATION",
                "confidence": 0.98
            }
        ],
        "facts": [
            {
                "local_id": "fact:alice-lives-in-shanghai",
                "subject_ref": "entity:alice",
                "predicate": "LIVES_IN",
                "object_ref": "entity:shanghai",
                "fact_text": "Alice lives in Shanghai.",
                "evidence": [
                    {
                        "text": "Alice lives in Shanghai.",
                        "start_byte": 3,
                        "end_byte": 10
                    }
                ],
                "confidence": 0.97,
                "valid_from_ms": null,
                "valid_to_ms": null
            }
        ]
    })
    .to_string();
    let extractor = LlmGraphExtractor::new(
        Arc::new(FakeGraphLlmClient::new(invalid_offsets_json)),
        GraphTypeRegistry::default(),
    );
    let executor = GraphExtractionExecutor::new(
        repo.clone(),
        Arc::new(extractor),
        GraphTypeRegistry::default(),
    );

    let extraction_run = executor
        .process_extraction_stage(&accepted.ingestion_run_id)
        .await
        .expect("exact evidence text should repair byte offsets");

    let stored = repo
        .get_extraction_run(&extraction_run.id, "space-a")
        .await
        .unwrap();
    let output = stored.structured_output.unwrap();
    let evidence = &output["facts"][0]["evidence"][0];
    assert_eq!(evidence["start_byte"], 0);
    assert_eq!(evidence["end_byte"], 24);
}

#[tokio::test]
async fn llm_graph_extractor_drops_facts_with_missing_entity_refs() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));
    let accepted = prepare_record_for_extraction(&repo).await;
    let missing_ref_json = serde_json::json!({
        "entities": [
            {
                "local_id": "entity:alice",
                "name": "Alice",
                "entity_type": "PERSON",
                "confidence": 0.99
            }
        ],
        "facts": [
            {
                "local_id": "fact:alice-lives-in-shanghai",
                "subject_ref": "entity:alice",
                "predicate": "LIVES_IN",
                "object_ref": "entity:shanghai",
                "fact_text": "Alice lives in Shanghai.",
                "evidence": [
                    {
                        "text": "Alice lives in Shanghai.",
                        "start_byte": 0,
                        "end_byte": 24
                    }
                ],
                "confidence": 0.97,
                "valid_from_ms": null,
                "valid_to_ms": null
            }
        ]
    })
    .to_string();
    let extractor = LlmGraphExtractor::new(
        Arc::new(FakeGraphLlmClient::new(missing_ref_json)),
        GraphTypeRegistry::default(),
    );
    let executor = GraphExtractionExecutor::new(
        repo.clone(),
        Arc::new(extractor),
        GraphTypeRegistry::default(),
    );

    let extraction_run = executor
        .process_extraction_stage(&accepted.ingestion_run_id)
        .await
        .expect("missing refs should drop only the invalid fact");

    let stored = repo
        .get_extraction_run(&extraction_run.id, "space-a")
        .await
        .unwrap();
    assert_eq!(stored.status, "completed");
    let output = stored.structured_output.unwrap();
    assert_eq!(output["entities"].as_array().unwrap().len(), 1);
    assert_eq!(output["facts"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn llm_graph_extractor_drops_facts_with_ungrounded_evidence_text() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));
    let accepted = prepare_record_for_extraction(&repo).await;
    let ungrounded_evidence_json = serde_json::json!({
        "entities": [
            {
                "local_id": "entity:alice",
                "name": "Alice",
                "entity_type": "PERSON",
                "confidence": 0.99
            },
            {
                "local_id": "entity:shanghai",
                "name": "Shanghai",
                "entity_type": "LOCATION",
                "confidence": 0.98
            }
        ],
        "facts": [
            {
                "local_id": "fact:alice-lives-in-shanghai",
                "subject_ref": "entity:alice",
                "predicate": "LIVES_IN",
                "object_ref": "entity:shanghai",
                "fact_text": "Alice lives in Shanghai.",
                "evidence": [
                    {
                        "text": "Alice moved to Shanghai.",
                        "start_byte": 0,
                        "end_byte": 24
                    }
                ],
                "confidence": 0.97,
                "valid_from_ms": null,
                "valid_to_ms": null
            }
        ]
    })
    .to_string();
    let extractor = LlmGraphExtractor::new(
        Arc::new(FakeGraphLlmClient::new(ungrounded_evidence_json)),
        GraphTypeRegistry::default(),
    );
    let executor = GraphExtractionExecutor::new(
        repo.clone(),
        Arc::new(extractor),
        GraphTypeRegistry::default(),
    );

    let extraction_run = executor
        .process_extraction_stage(&accepted.ingestion_run_id)
        .await
        .expect("ungrounded evidence should drop only the invalid fact");

    let stored = repo
        .get_extraction_run(&extraction_run.id, "space-a")
        .await
        .unwrap();
    assert_eq!(stored.status, "completed");
    let output = stored.structured_output.unwrap();
    assert_eq!(output["entities"].as_array().unwrap().len(), 2);
    assert_eq!(output["facts"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn llm_graph_extractor_rejects_invalid_json_response() {
    let extractor = LlmGraphExtractor::new(
        Arc::new(FakeGraphLlmClient::new("not-json")),
        GraphTypeRegistry::default(),
    );

    let error = extractor
        .extract(memory_core::graph::GraphExtractionInput {
            memory_space_id: "space-a".to_string(),
            memory_record_id: "record-a".to_string(),
            text: "Alice lives in Shanghai.".to_string(),
            metadata: serde_json::json!({}),
            context_record_ids: vec!["record-a".to_string()],
            type_registry_version: "graph-type-registry-v1".to_string(),
        })
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("failed to parse graph extraction JSON"));
}

#[tokio::test]
async fn llm_graph_extractor_drops_a_fact_with_a_null_endpoint() {
    let response = serde_json::json!({
        "entities": [
            {
                "local_id": "entity:alice",
                "name": "Alice",
                "entity_type": "PERSON",
                "confidence": 1.0
            },
            {
                "local_id": "entity:shanghai",
                "name": "Shanghai",
                "entity_type": "LOCATION",
                "confidence": 1.0
            }
        ],
        "facts": [
            {
                "local_id": "fact:missing-object",
                "subject_ref": "entity:alice",
                "predicate": "LIVES_IN",
                "object_ref": null,
                "fact_text": "Alice lives somewhere.",
                "evidence": [{"text": "Alice lives in Shanghai.", "start_byte": 0, "end_byte": 24}],
                "confidence": 1.0
            },
            {
                "local_id": "fact:alice-shanghai",
                "subject_ref": "entity:alice",
                "predicate": "LIVES_IN",
                "object_ref": "entity:shanghai",
                "fact_text": "Alice lives in Shanghai.",
                "evidence": [{"text": "Alice lives in Shanghai.", "start_byte": 0, "end_byte": 24}],
                "confidence": 1.0
            }
        ]
    })
    .to_string();
    let extractor = LlmGraphExtractor::new(
        Arc::new(FakeGraphLlmClient::new(response.clone())),
        GraphTypeRegistry::default(),
    );

    let output = extractor
        .extract(memory_core::graph::GraphExtractionInput {
            memory_space_id: "space-a".to_string(),
            memory_record_id: "record-a".to_string(),
            text: "Alice lives in Shanghai.".to_string(),
            metadata: serde_json::json!({}),
            context_record_ids: vec!["record-a".to_string()],
            type_registry_version: "graph-type-registry-v1".to_string(),
        })
        .await
        .unwrap();

    assert_eq!(output.entities.len(), 2);
    assert_eq!(output.facts.len(), 1);
    assert_eq!(output.facts[0].local_id, "fact:alice-shanghai");
    assert!(memory_core::graph::parse_graph_extraction_output_text(&response).is_err());
}

#[tokio::test]
async fn llm_graph_extractor_output_still_goes_through_candidate_validation() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));
    let accepted = prepare_record_for_extraction(&repo).await;
    let invalid_json = serde_json::json!({
        "entities": [
            {
                "local_id": "entity:alice",
                "name": "Alice",
                "entity_type": "PERSON",
                "confidence": 0.99
            },
            {
                "local_id": "entity:shanghai",
                "name": "Shanghai",
                "entity_type": "PLANET",
                "confidence": 0.98
            }
        ],
        "facts": [
            {
                "local_id": "fact:invalid",
                "subject_ref": "entity:alice",
                "predicate": "LIVES_IN",
                "object_ref": "entity:shanghai",
                "fact_text": "Alice lives in Shanghai.",
                "evidence": [
                    {
                        "text": "Alice lives in Shanghai.",
                        "start_byte": 0,
                        "end_byte": 24
                    }
                ],
                "confidence": 0.97
            }
        ]
    })
    .to_string();
    let extractor = LlmGraphExtractor::new(
        Arc::new(FakeGraphLlmClient::new(invalid_json)),
        GraphTypeRegistry::default(),
    );
    let executor = GraphExtractionExecutor::new(
        repo.clone(),
        Arc::new(extractor),
        GraphTypeRegistry::default(),
    );

    let error = executor
        .process_extraction_stage(&accepted.ingestion_run_id)
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("INVALID_EXTRACTION_OUTPUT"));
    let run = repo
        .get_run(&accepted.ingestion_run_id, "space-a")
        .await
        .unwrap();
    assert_eq!(run.status, "failed");
    assert_eq!(run.error_code.as_deref(), Some("INVALID_EXTRACTION_OUTPUT"));
}

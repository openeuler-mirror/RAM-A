use std::sync::{Arc, Mutex};

use memory_core::{
    graph::{
        GraphExtractionExecutor, GraphExtractor, GraphLlmClient, GraphLlmRequest, GraphLlmResponse,
        GraphTypeRegistry, LlmGraphExtractor, GRAPH_EXTRACTION_PROMPT_VERSION,
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
    assert_eq!(stored.prompt_version, GRAPH_EXTRACTION_PROMPT_VERSION);
    assert_eq!(stored.schema_version, "graph-extraction-candidates-v3");
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
    assert!(user_prompt.contains("Do not extract conversational bookkeeping"));
    assert!(user_prompt.contains("source_context.speaker"));
    assert!(user_prompt.contains("temporal_expression"));
    assert!(user_prompt.contains("Do not return valid_from_ms or valid_to_ms"));
    assert!(!user_prompt.contains("\"valid_from_ms\": null"));
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
async fn llm_graph_extractor_includes_source_context_for_first_person_grounding() {
    let fake_client = Arc::new(FakeGraphLlmClient::new(
        serde_json::json!({"entities": [], "facts": []}).to_string(),
    ));
    let extractor = LlmGraphExtractor::new(fake_client.clone(), GraphTypeRegistry::default());

    let output = extractor
        .extract(memory_core::graph::GraphExtractionInput {
            memory_space_id: "space-a".to_string(),
            memory_record_id: "record-a".to_string(),
            text: "I'm going to a conference this month.".to_string(),
            metadata: serde_json::json!({
                "speaker": "Alice",
                "session_id": "session-1",
                "turn_index": 3,
                "session_timestamp": "10:00 am on 8 May, 2023",
                "observed_at_ms": 1683511200000u64,
                "unrelated": "kept in metadata but not source_context"
            }),
            context_record_ids: vec!["record-a".to_string()],
            type_registry_version: "graph-type-registry-v1".to_string(),
        })
        .await
        .unwrap();

    assert!(output.facts.is_empty());
    let requests = fake_client.requests();
    let user_prompt = requests[0]
        .messages
        .iter()
        .find(|message| message.role == "user")
        .unwrap()
        .content
        .as_str();
    assert!(user_prompt.contains("\"source_context\""));
    assert!(user_prompt.contains("\"speaker\": \"Alice\""));
    assert!(user_prompt.contains("\"observed_at_ms\": 1683511200000"));
    assert!(user_prompt.contains("first-person statements"));
    assert!(user_prompt.contains("do not create facts from metadata alone"));
}

fn temporal_llm_json(
    predicate: &str,
    fact_text: &str,
    evidence_text: &str,
    temporal_expression: Option<&str>,
    valid_from_ms: Option<u64>,
    valid_to_ms: Option<u64>,
) -> String {
    serde_json::json!({
        "entities": [
            {
                "local_id": "entity:alice",
                "name": "Alice",
                "entity_type": "PERSON",
                "confidence": 0.99
            },
            {
                "local_id": "entity:paris",
                "name": "Paris",
                "entity_type": "LOCATION",
                "confidence": 0.98
            }
        ],
        "facts": [
            {
                "local_id": "fact:alice-paris",
                "subject_ref": "entity:alice",
                "predicate": predicate,
                "object_ref": "entity:paris",
                "fact_text": fact_text,
                "evidence": [
                    {
                        "text": evidence_text,
                        "start_byte": 0,
                        "end_byte": evidence_text.len()
                    }
                ],
                "confidence": 0.97,
                "temporal_expression": temporal_expression,
                "valid_from_ms": valid_from_ms,
                "valid_to_ms": valid_to_ms
            }
        ]
    })
    .to_string()
}

#[tokio::test]
async fn llm_graph_extractor_discards_llm_event_time_without_temporal_expression() {
    let record_text = "Alice visited Paris.";
    let observed_at_ms = 1_683_511_200_000;
    let extractor = LlmGraphExtractor::new(
        Arc::new(FakeGraphLlmClient::new(temporal_llm_json(
            "VISITED",
            record_text,
            record_text,
            None,
            Some(observed_at_ms),
            None,
        ))),
        GraphTypeRegistry::default(),
    );

    let output = extractor
        .extract(memory_core::graph::GraphExtractionInput {
            memory_space_id: "space-a".to_string(),
            memory_record_id: "record-a".to_string(),
            text: record_text.to_string(),
            metadata: serde_json::json!({"observed_at_ms": observed_at_ms}),
            context_record_ids: vec!["record-a".to_string()],
            type_registry_version: "graph-type-registry-v1".to_string(),
        })
        .await
        .unwrap();

    assert_eq!(output.facts.len(), 1);
    assert_eq!(output.facts[0].valid_from_ms, None);
    assert_eq!(output.facts[0].valid_to_ms, None);
}

#[tokio::test]
async fn llm_graph_extractor_keeps_grounded_event_expression_but_discards_llm_time() {
    let record_text = "Alice visited Paris yesterday.";
    let resolved_time_ms = 1_683_424_800_000;
    let extractor = LlmGraphExtractor::new(
        Arc::new(FakeGraphLlmClient::new(temporal_llm_json(
            "VISITED",
            record_text,
            record_text,
            Some("yesterday"),
            Some(resolved_time_ms),
            Some(resolved_time_ms),
        ))),
        GraphTypeRegistry::default(),
    );

    let output = extractor
        .extract(memory_core::graph::GraphExtractionInput {
            memory_space_id: "space-a".to_string(),
            memory_record_id: "record-a".to_string(),
            text: record_text.to_string(),
            metadata: serde_json::json!({"observed_at_ms": 1_683_511_200_000_u64}),
            context_record_ids: vec!["record-a".to_string()],
            type_registry_version: "graph-type-registry-v1".to_string(),
        })
        .await
        .unwrap();

    assert_eq!(output.facts.len(), 1);
    assert_eq!(
        output.facts[0].temporal_expression.as_deref(),
        Some("yesterday")
    );
    assert_eq!(output.facts[0].valid_from_ms, None);
    assert_eq!(output.facts[0].valid_to_ms, None);
}

#[tokio::test]
async fn llm_graph_extractor_discards_llm_state_time() {
    let record_text = "Alice lives in Paris.";
    let observed_at_ms = 1_683_511_200_000;
    let extractor = LlmGraphExtractor::new(
        Arc::new(FakeGraphLlmClient::new(temporal_llm_json(
            "LIVES_IN",
            record_text,
            record_text,
            None,
            Some(observed_at_ms),
            None,
        ))),
        GraphTypeRegistry::default(),
    );

    let output = extractor
        .extract(memory_core::graph::GraphExtractionInput {
            memory_space_id: "space-a".to_string(),
            memory_record_id: "record-a".to_string(),
            text: record_text.to_string(),
            metadata: serde_json::json!({"observed_at_ms": observed_at_ms}),
            context_record_ids: vec!["record-a".to_string()],
            type_registry_version: "graph-type-registry-v1".to_string(),
        })
        .await
        .unwrap();

    assert_eq!(output.facts.len(), 1);
    assert_eq!(output.facts[0].valid_from_ms, None);
    assert_eq!(output.facts[0].valid_to_ms, None);
}

#[tokio::test]
async fn llm_graph_extractor_drops_time_with_ungrounded_temporal_expression() {
    let record_text = "Alice visited Paris.";
    let extractor = LlmGraphExtractor::new(
        Arc::new(FakeGraphLlmClient::new(temporal_llm_json(
            "VISITED",
            record_text,
            record_text,
            Some("yesterday"),
            Some(1_683_424_800_000),
            Some(1_683_424_800_000),
        ))),
        GraphTypeRegistry::default(),
    );

    let output = extractor
        .extract(memory_core::graph::GraphExtractionInput {
            memory_space_id: "space-a".to_string(),
            memory_record_id: "record-a".to_string(),
            text: record_text.to_string(),
            metadata: serde_json::json!({"observed_at_ms": 1_683_511_200_000_u64}),
            context_record_ids: vec!["record-a".to_string()],
            type_registry_version: "graph-type-registry-v1".to_string(),
        })
        .await
        .unwrap();

    assert_eq!(output.facts.len(), 1);
    assert_eq!(output.facts[0].temporal_expression, None);
    assert_eq!(output.facts[0].valid_from_ms, None);
    assert_eq!(output.facts[0].valid_to_ms, None);
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
async fn llm_graph_extractor_falls_back_unknown_predicates_to_related_to() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));
    let accepted = prepare_record_for_extraction(&repo).await;
    let unknown_predicate_json = serde_json::json!({
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
                "local_id": "fact:alice-moved-to-shanghai",
                "subject_ref": "entity:alice",
                "predicate": "MOVED_TO",
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
        Arc::new(FakeGraphLlmClient::new(unknown_predicate_json)),
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
        .expect("unknown predicate should fall back to RELATED_TO");

    let stored = repo
        .get_extraction_run(&extraction_run.id, "space-a")
        .await
        .unwrap();
    assert_eq!(stored.status, "completed");
    let output = stored.structured_output.unwrap();
    assert_eq!(output["facts"].as_array().unwrap().len(), 1);
    assert_eq!(output["facts"][0]["predicate"], "RELATED_TO");
}

#[tokio::test]
async fn llm_graph_extractor_drops_low_signal_conversational_facts() {
    let noisy_json = serde_json::json!({
        "entities": [
            {
                "local_id": "entity:alice",
                "name": "Alice",
                "entity_type": "PERSON",
                "confidence": 0.99
            },
            {
                "local_id": "entity:bob",
                "name": "Bob",
                "entity_type": "PERSON",
                "confidence": 0.98
            },
            {
                "local_id": "entity:shanghai",
                "name": "Shanghai",
                "entity_type": "LOCATION",
                "confidence": 0.97
            }
        ],
        "facts": [
            {
                "local_id": "fact:alice-thanked-bob",
                "subject_ref": "entity:alice",
                "predicate": "FRIEND_OF",
                "object_ref": "entity:bob",
                "fact_text": "Alice thanked Bob.",
                "evidence": [{"text": "Alice thanked Bob.", "start_byte": 0, "end_byte": 18}],
                "confidence": 0.93
            },
            {
                "local_id": "fact:bob-mentioned-alice",
                "subject_ref": "entity:bob",
                "predicate": "MENTIONED",
                "object_ref": "entity:alice",
                "fact_text": "Bob mentioned Alice.",
                "evidence": [{"text": "Bob mentioned Alice.", "start_byte": 19, "end_byte": 39}],
                "confidence": 0.91
            },
            {
                "local_id": "fact:alice-lives-in-shanghai",
                "subject_ref": "entity:alice",
                "predicate": "LIVES_IN",
                "object_ref": "entity:shanghai",
                "fact_text": "Alice lives in Shanghai.",
                "evidence": [{"text": "Alice lives in Shanghai.", "start_byte": 40, "end_byte": 64}],
                "confidence": 0.97
            }
        ]
    })
    .to_string();
    let extractor = LlmGraphExtractor::new(
        Arc::new(FakeGraphLlmClient::new(noisy_json)),
        GraphTypeRegistry::default(),
    );

    let output = extractor
        .extract(memory_core::graph::GraphExtractionInput {
            memory_space_id: "space-a".to_string(),
            memory_record_id: "record-a".to_string(),
            text: "Alice thanked Bob. Bob mentioned Alice. Alice lives in Shanghai.".to_string(),
            metadata: serde_json::json!({}),
            context_record_ids: vec!["record-a".to_string()],
            type_registry_version: "graph-type-registry-v1".to_string(),
        })
        .await
        .unwrap();

    assert_eq!(output.facts.len(), 1);
    assert_eq!(output.facts[0].fact_text, "Alice lives in Shanghai.");
    assert_eq!(output.facts[0].predicate, "LIVES_IN");
}

#[tokio::test]
async fn llm_graph_extractor_deduplicates_same_fact_text_by_predicate_strength() {
    let duplicate_json = serde_json::json!({
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
                "local_id": "fact:alice-related-shanghai",
                "subject_ref": "entity:alice",
                "predicate": "RELATED_TO",
                "object_ref": "entity:shanghai",
                "fact_text": "Alice lives in Shanghai.",
                "evidence": [{"text": "Alice lives in Shanghai.", "start_byte": 0, "end_byte": 24}],
                "confidence": 0.99
            },
            {
                "local_id": "fact:alice-lives-in-shanghai",
                "subject_ref": "entity:alice",
                "predicate": "LIVES_IN",
                "object_ref": "entity:shanghai",
                "fact_text": "Alice lives in Shanghai.",
                "evidence": [{"text": "Alice lives in Shanghai.", "start_byte": 0, "end_byte": 24}],
                "confidence": 0.90
            }
        ]
    })
    .to_string();
    let extractor = LlmGraphExtractor::new(
        Arc::new(FakeGraphLlmClient::new(duplicate_json)),
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

    assert_eq!(output.facts.len(), 1);
    assert_eq!(output.facts[0].predicate, "LIVES_IN");
    assert_eq!(output.facts[0].fact_text, "Alice lives in Shanghai.");
}

#[tokio::test]
async fn llm_graph_extractor_repairs_duplicate_fact_local_ids() {
    let duplicate_id_json = serde_json::json!({
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
            },
            {
                "local_id": "entity:painting",
                "name": "painting",
                "entity_type": "ACTIVITY",
                "confidence": 0.97
            }
        ],
        "facts": [
            {
                "local_id": "fact:alice-memory",
                "subject_ref": "entity:alice",
                "predicate": "LIVES_IN",
                "object_ref": "entity:shanghai",
                "fact_text": "Alice lives in Shanghai.",
                "evidence": [{"text": "Alice lives in Shanghai.", "start_byte": 0, "end_byte": 24}],
                "confidence": 0.97
            },
            {
                "local_id": "fact:alice-memory",
                "subject_ref": "entity:alice",
                "predicate": "LIKES",
                "object_ref": "entity:painting",
                "fact_text": "Alice likes painting.",
                "evidence": [{"text": "Alice likes painting.", "start_byte": 25, "end_byte": 46}],
                "confidence": 0.96
            }
        ]
    })
    .to_string();
    let extractor = LlmGraphExtractor::new(
        Arc::new(FakeGraphLlmClient::new(duplicate_id_json)),
        GraphTypeRegistry::default(),
    );

    let output = extractor
        .extract(memory_core::graph::GraphExtractionInput {
            memory_space_id: "space-a".to_string(),
            memory_record_id: "record-a".to_string(),
            text: "Alice lives in Shanghai. Alice likes painting.".to_string(),
            metadata: serde_json::json!({}),
            context_record_ids: vec!["record-a".to_string()],
            type_registry_version: "graph-type-registry-v1".to_string(),
        })
        .await
        .unwrap();

    assert_eq!(output.facts.len(), 2);
    assert_eq!(output.facts[0].local_id, "fact:alice-memory");
    assert_eq!(output.facts[1].local_id, "fact:alice-memory:2");
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
async fn llm_graph_extractor_normalizes_unknown_entity_types_before_validation() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));
    let accepted = prepare_record_for_extraction(&repo).await;
    let unknown_type_json = serde_json::json!({
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
        Arc::new(FakeGraphLlmClient::new(unknown_type_json)),
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
        .unwrap();

    let stored = repo
        .get_extraction_run(&extraction_run.id, "space-a")
        .await
        .unwrap();
    assert_eq!(stored.status, "completed");
    let output = stored.structured_output.unwrap();
    assert_eq!(output["entities"][1]["entity_type"], "CONCEPT");
    assert_eq!(output["facts"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn llm_graph_extractor_maps_common_entity_type_aliases() {
    let family_json = serde_json::json!({
        "entities": [
            {
                "local_id": "entity:alice",
                "name": "Alice",
                "entity_type": "PERSON",
                "confidence": 0.99
            },
            {
                "local_id": "entity:family",
                "name": "family",
                "entity_type": "FAMILY",
                "confidence": 0.98
            }
        ],
        "facts": [
            {
                "local_id": "fact:alice-cares-for-family",
                "subject_ref": "entity:alice",
                "predicate": "RELATED_TO",
                "object_ref": "entity:family",
                "fact_text": "Alice looks after her family.",
                "evidence": [
                    {
                        "text": "Alice looks after her family.",
                        "start_byte": 0,
                        "end_byte": 29
                    }
                ],
                "confidence": 0.97
            }
        ]
    })
    .to_string();
    let extractor = LlmGraphExtractor::new(
        Arc::new(FakeGraphLlmClient::new(family_json)),
        GraphTypeRegistry::default(),
    );

    let output = extractor
        .extract(memory_core::graph::GraphExtractionInput {
            memory_space_id: "space-a".to_string(),
            memory_record_id: "record-a".to_string(),
            text: "Alice looks after her family.".to_string(),
            metadata: serde_json::json!({}),
            context_record_ids: vec!["record-a".to_string()],
            type_registry_version: "graph-type-registry-v1".to_string(),
        })
        .await
        .unwrap();

    assert_eq!(output.entities[1].entity_type, "GROUP");
}

use std::sync::Arc;

use memory_core::{
    graph::GraphIngestionExecutor, sqlite::GraphRepository, EmbeddingProvider,
    GraphAddMemoryRequest, GraphAddMemoryResponse,
};

#[derive(Debug)]
struct FixedEmbedding;

#[async_trait::async_trait]
impl EmbeddingProvider for FixedEmbedding {
    fn dimensions(&self) -> usize {
        3
    }

    fn model_name(&self) -> &str {
        "dataset-smoke-embedding"
    }

    async fn embed(&self, texts: &[String]) -> memory_core::MemoryResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.25, 0.5, 0.75]).collect())
    }
}

fn dataset_shaped_request(index: usize, text: &str) -> GraphAddMemoryRequest {
    GraphAddMemoryRequest {
        memory_space_id: "personalmem-user-001".to_string(),
        owner_id: "personalmem-user-001".to_string(),
        idempotency_key: format!("prepared-memory-{index}"),
        text: text.to_string(),
        metadata: serde_json::json!({
            "dataset": "dataset-shaped-smoke",
            "scope_id": "conversation-001",
            "turn_index": index,
        }),
        session_id: Some("conversation-001".to_string()),
        session_sequence: Some(index as i64),
        source_kind: "prepared_dataset_memory".to_string(),
        source_ref: Some(format!("prepared/personalmem_32k_v1.json#memory-{index}")),
        content_role: "user".to_string(),
        created_by_agent_id: None,
        observed_at_ms: Some(1_720_000_000_000 + index as u64),
    }
}

async fn ingest_dataset_shaped_samples(
    repo: &GraphRepository,
    executor: &GraphIngestionExecutor,
    samples: &[&str],
) -> Vec<GraphAddMemoryResponse> {
    let mut accepted = Vec::with_capacity(samples.len());
    for (index, sample) in samples.iter().enumerate() {
        let response = repo
            .accept_memory_record(dataset_shaped_request(index + 1, sample))
            .await
            .unwrap();
        executor
            .process_vector_stage(&response.ingestion_run_id)
            .await
            .unwrap();
        accepted.push(response);
    }
    accepted
}

#[tokio::test]
async fn dataset_shaped_memories_flow_through_graph_add_and_embedding() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GraphRepository::open(temp.path().join("graph.sqlite"));
    let executor = GraphIngestionExecutor::new(repo.clone(), Arc::new(FixedEmbedding));
    let samples = [
        "The user prefers quiet vegan restaurants near their workplace.",
        "The user moved from Shanghai to Hangzhou after changing jobs.",
        "The user now wants reminders about medication after breakfast.",
    ];

    let accepted = ingest_dataset_shaped_samples(&repo, &executor, &samples).await;

    assert_eq!(accepted.len(), samples.len());
    for (index, response) in accepted.iter().enumerate() {
        let record = repo
            .get_graph_memory_record(&response.memory_record_id, "personalmem-user-001")
            .await
            .unwrap();
        assert_eq!(record.text, samples[index]);
        assert_eq!(record.embedding, Some(vec![0.25, 0.5, 0.75]));
        assert_eq!(
            record.embedding_model.as_deref(),
            Some("dataset-smoke-embedding")
        );

        let run = repo
            .get_run(&response.ingestion_run_id, "personalmem-user-001")
            .await
            .unwrap();
        assert_eq!(run.status, "running");
        assert_eq!(run.stage, "extraction");
    }
}

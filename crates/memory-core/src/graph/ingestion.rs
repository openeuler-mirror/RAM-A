use std::sync::Arc;

use crate::{
    sqlite::{GraphRepository, RecordEmbeddingUpdate},
    EmbeddingProvider, MemoryResult,
};

const GRAPH_EMBEDDING_VERSION: &str = "graph-embedding-v1";

pub struct GraphIngestionExecutor {
    repository: GraphRepository,
    embedder: Arc<dyn EmbeddingProvider>,
}

impl GraphIngestionExecutor {
    pub fn new(repository: GraphRepository, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        Self {
            repository,
            embedder,
        }
    }

    pub async fn process_vector_stage(&self, ingestion_run_id: &str) -> MemoryResult<()> {
        let claim = self.repository.claim_pending_run(ingestion_run_id).await?;
        let embedding = match self.embedder.embed_one(&claim.text).await {
            Ok(embedding) => embedding,
            Err(error) => {
                let error_message = error.to_string();
                let _ = self
                    .repository
                    .mark_run_failed_if_current_attempt(
                        ingestion_run_id,
                        claim.attempt_count,
                        "embedding",
                        "EMBEDDING_FAILED",
                        &error_message,
                    )
                    .await;
                return Err(error);
            }
        };

        let result = self
            .repository
            .store_record_embedding(RecordEmbeddingUpdate {
                ingestion_run_id: ingestion_run_id.to_string(),
                memory_record_id: claim.memory_record_id.clone(),
                memory_space_id: claim.memory_space_id.clone(),
                attempt_count: claim.attempt_count,
                embedding,
                embedding_model: self.embedder.model_name().to_string(),
                embedding_version: GRAPH_EMBEDDING_VERSION.to_string(),
            })
            .await;

        if let Err(error) = result {
            let error_message = error.to_string();
            let _ = self
                .repository
                .mark_run_failed_if_current_attempt(
                    ingestion_run_id,
                    claim.attempt_count,
                    "embedding",
                    "EMBEDDING_STORE_FAILED",
                    &error_message,
                )
                .await;
            return Err(error);
        }

        Ok(())
    }
}

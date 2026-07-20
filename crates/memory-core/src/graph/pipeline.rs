use std::sync::Arc;

use crate::sqlite::GraphRepository;
use crate::{
    EmbeddingProvider, GraphAddMemoryRequest, GraphAddMemoryResponse, MemoryError, MemoryResult,
};

use super::{
    ExtractionRun, GraphExtractionExecutor, GraphExtractor, GraphIngestionExecutor,
    GraphResolutionExecutor, GraphResolutionResult, GraphTypeRegistry, IngestionRun,
};

pub struct GraphBuildPipeline {
    repository: GraphRepository,
    ingestion: GraphIngestionExecutor,
    extraction: GraphExtractionExecutor,
    resolution: GraphResolutionExecutor,
}

#[derive(Clone, Debug)]
pub struct GraphBuildResult {
    pub add_response: GraphAddMemoryResponse,
    pub ingestion_run: IngestionRun,
    pub extraction_run: ExtractionRun,
    pub resolution: GraphResolutionResult,
}

impl GraphBuildPipeline {
    pub fn new(
        repository: GraphRepository,
        embedder: Arc<dyn EmbeddingProvider>,
        extractor: Arc<dyn GraphExtractor>,
        type_registry: GraphTypeRegistry,
    ) -> Self {
        Self {
            ingestion: GraphIngestionExecutor::new(repository.clone(), embedder),
            extraction: GraphExtractionExecutor::new(
                repository.clone(),
                extractor,
                type_registry.clone(),
            ),
            resolution: GraphResolutionExecutor::new(repository.clone(), type_registry),
            repository,
        }
    }

    pub async fn build_memory(
        &self,
        request: GraphAddMemoryRequest,
    ) -> MemoryResult<GraphBuildResult> {
        let add_response = self.repository.accept_memory_record(request).await?;
        self.ingestion
            .process_vector_stage(&add_response.ingestion_run_id)
            .await?;
        let extraction_run = self
            .extraction
            .process_extraction_stage(&add_response.ingestion_run_id)
            .await?;
        let resolution = self
            .resolution
            .process_resolution_stage(&add_response.ingestion_run_id)
            .await?;

        Ok(GraphBuildResult {
            add_response,
            ingestion_run: resolution.ingestion_run.clone(),
            extraction_run,
            resolution,
        })
    }

    pub async fn build_memory_if_needed(
        &self,
        request: GraphAddMemoryRequest,
    ) -> MemoryResult<Option<GraphBuildResult>> {
        let memory_space_id = request.memory_space_id.clone();
        let add_response = self.repository.accept_memory_record(request).await?;
        if add_response.status == "completed" {
            return Ok(None);
        }
        if add_response.status != "pending" {
            let run = self
                .repository
                .get_run(&add_response.ingestion_run_id, &memory_space_id)
                .await?;
            return Err(MemoryError::StoreBackend {
                message: format!(
                    "graph ingestion run {} is not resumable from status `{}` stage `{}`",
                    run.id, run.status, run.stage
                ),
            });
        }

        self.ingestion
            .process_vector_stage(&add_response.ingestion_run_id)
            .await?;
        let extraction_run = self
            .extraction
            .process_extraction_stage(&add_response.ingestion_run_id)
            .await?;
        let resolution = self
            .resolution
            .process_resolution_stage(&add_response.ingestion_run_id)
            .await?;

        Ok(Some(GraphBuildResult {
            add_response,
            ingestion_run: resolution.ingestion_run.clone(),
            extraction_run,
            resolution,
        }))
    }
}

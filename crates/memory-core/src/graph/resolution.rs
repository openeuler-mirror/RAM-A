use crate::sqlite::{GraphRepository, ResolutionPublishRequest};
use crate::MemoryResult;

use super::{GraphTypeRegistry, IngestionRun};

pub const GRAPH_RESOLVER_VERSION: &str = "graph-resolution-v1";
pub const RESOLUTION_FAILED_ERROR_CODE: &str = "RESOLUTION_FAILED";
pub const RESOLUTION_STORE_FAILED_ERROR_CODE: &str = "RESOLUTION_STORE_FAILED";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphResolutionResult {
    pub ingestion_run: IngestionRun,
    pub entities_created: usize,
    pub entities_reused: usize,
    pub facts_created: usize,
    pub facts_reused: usize,
    pub evidence_inserted: usize,
}

pub struct GraphResolutionExecutor {
    repository: GraphRepository,
    type_registry: GraphTypeRegistry,
}

impl GraphResolutionExecutor {
    pub fn new(repository: GraphRepository, type_registry: GraphTypeRegistry) -> Self {
        Self {
            repository,
            type_registry,
        }
    }

    pub async fn process_resolution_stage(
        &self,
        ingestion_run_id: &str,
    ) -> MemoryResult<GraphResolutionResult> {
        let claim = self
            .repository
            .claim_resolution_run(ingestion_run_id)
            .await?;
        if let Err(error) = claim
            .extraction_output
            .validate_against_record(&claim.text, &self.type_registry)
        {
            let _ = self
                .repository
                .mark_run_failed_if_current_attempt(
                    ingestion_run_id,
                    claim.attempt_count,
                    "resolving",
                    RESOLUTION_FAILED_ERROR_CODE,
                    &error.to_string(),
                )
                .await;
            return Err(error);
        }

        let publish_result = self
            .repository
            .publish_resolution(ResolutionPublishRequest {
                ingestion_run_id: claim.ingestion_run_id.clone(),
                memory_space_id: claim.memory_space_id.clone(),
                memory_record_id: claim.memory_record_id.clone(),
                extraction_run_id: claim.extraction_run_id.clone(),
                attempt_count: claim.attempt_count,
                extraction_output: claim.extraction_output.clone(),
                type_registry_version: self.type_registry.version.clone(),
                resolver_version: GRAPH_RESOLVER_VERSION.to_string(),
            })
            .await;

        match publish_result {
            Ok(result) => Ok(GraphResolutionResult {
                ingestion_run: result.ingestion_run,
                entities_created: result.entities_created,
                entities_reused: result.entities_reused,
                facts_created: result.facts_created,
                facts_reused: result.facts_reused,
                evidence_inserted: result.evidence_inserted,
            }),
            Err(error) => {
                let _ = self
                    .repository
                    .mark_run_failed_if_current_attempt(
                        ingestion_run_id,
                        claim.attempt_count,
                        "resolving",
                        RESOLUTION_STORE_FAILED_ERROR_CODE,
                        &error.to_string(),
                    )
                    .await;
                Err(error)
            }
        }
    }
}

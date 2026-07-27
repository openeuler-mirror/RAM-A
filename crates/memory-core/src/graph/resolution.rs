use crate::sqlite::{GraphRepository, ResolutionPublishRequest};
use crate::MemoryResult;

use super::{GraphSourceEntity, GraphTypeRegistry, IngestionRun};

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
    pub record_entity_links_inserted: usize,
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
        let source_entity = match source_entity_from_metadata(&claim.metadata, &self.type_registry)
        {
            Ok(source_entity) => source_entity,
            Err(error) => {
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
        };

        let publish_result = self
            .repository
            .publish_resolution(ResolutionPublishRequest {
                ingestion_run_id: claim.ingestion_run_id.clone(),
                memory_space_id: claim.memory_space_id.clone(),
                memory_record_id: claim.memory_record_id.clone(),
                extraction_run_id: claim.extraction_run_id.clone(),
                attempt_count: claim.attempt_count,
                extraction_output: claim.extraction_output.clone(),
                source_entity,
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
                record_entity_links_inserted: result.record_entity_links_inserted,
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

fn source_entity_from_metadata(
    metadata: &serde_json::Value,
    type_registry: &GraphTypeRegistry,
) -> MemoryResult<Option<GraphSourceEntity>> {
    let Some(value) = metadata.get("graph_source_entity") else {
        return Ok(None);
    };
    let Some(object) = value.as_object() else {
        return Err(crate::MemoryError::InvalidInput {
            message: "graph_source_entity metadata must be an object".to_string(),
        });
    };
    let name = object
        .get("name")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| crate::MemoryError::InvalidInput {
            message: "graph_source_entity.name must be a non-empty string".to_string(),
        })?;
    let entity_type = object
        .get("entity_type")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| crate::MemoryError::InvalidInput {
            message: "graph_source_entity.entity_type must be a non-empty string".to_string(),
        })?
        .to_ascii_uppercase();
    if !type_registry
        .entity_types
        .iter()
        .any(|registered| registered == &entity_type)
    {
        return Err(crate::MemoryError::InvalidInput {
            message: format!("graph_source_entity.entity_type `{entity_type}` is not registered"),
        });
    }

    Ok(Some(GraphSourceEntity {
        name: name.to_string(),
        entity_type,
    }))
}

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::sqlite::{ExtractionRunCompletion, ExtractionRunFailure, GraphRepository};
use crate::{MemoryError, MemoryResult};

use super::{ExtractionRun, GraphTypeRegistry};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphExtractionInput {
    pub memory_space_id: String,
    pub memory_record_id: String,
    pub text: String,
    pub metadata: serde_json::Value,
    pub context_record_ids: Vec<String>,
    pub type_registry_version: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtractedEntityCandidate {
    pub local_id: String,
    pub name: String,
    pub entity_type: String,
    pub confidence: Option<f32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphEvidenceSpan {
    pub text: Option<String>,
    pub start_byte: Option<usize>,
    pub end_byte: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtractedFactCandidate {
    pub local_id: String,
    pub subject_ref: String,
    pub predicate: String,
    pub object_ref: String,
    pub fact_text: String,
    pub evidence: Vec<GraphEvidenceSpan>,
    pub confidence: Option<f32>,
    pub valid_from_ms: Option<u64>,
    pub valid_to_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphExtractionOutput {
    pub entities: Vec<ExtractedEntityCandidate>,
    pub facts: Vec<ExtractedFactCandidate>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

impl GraphExtractionOutput {
    pub fn validate_against_record(
        &self,
        record_text: &str,
        type_registry: &GraphTypeRegistry,
    ) -> MemoryResult<()> {
        let entity_types = type_registry
            .entity_types
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let mut entity_ids = HashSet::new();
        for entity in &self.entities {
            validate_non_empty("entity.local_id", &entity.local_id)?;
            validate_non_empty("entity.name", &entity.name)?;
            validate_non_empty("entity.entity_type", &entity.entity_type)?;
            validate_confidence("entity.confidence", entity.confidence)?;
            if !entity_types.contains(entity.entity_type.as_str()) {
                return invalid_output(format!("unknown entity type '{}'", entity.entity_type));
            }
            if !entity_ids.insert(entity.local_id.as_str()) {
                return invalid_output(format!("duplicate entity local_id '{}'", entity.local_id));
            }
        }

        let mut fact_ids = HashSet::new();
        for fact in &self.facts {
            validate_non_empty("fact.local_id", &fact.local_id)?;
            validate_non_empty("fact.subject_ref", &fact.subject_ref)?;
            validate_non_empty("fact.predicate", &fact.predicate)?;
            validate_non_empty("fact.object_ref", &fact.object_ref)?;
            validate_non_empty("fact.fact_text", &fact.fact_text)?;
            validate_confidence("fact.confidence", fact.confidence)?;
            if !fact_ids.insert(fact.local_id.as_str()) {
                return invalid_output(format!("duplicate fact local_id '{}'", fact.local_id));
            }
            if !entity_ids.contains(fact.subject_ref.as_str()) {
                return invalid_output(format!(
                    "fact '{}' references unknown subject '{}'",
                    fact.local_id, fact.subject_ref
                ));
            }
            if !entity_ids.contains(fact.object_ref.as_str()) {
                return invalid_output(format!(
                    "fact '{}' references unknown object '{}'",
                    fact.local_id, fact.object_ref
                ));
            }
            if type_registry.predicate(&fact.predicate).is_none() {
                return invalid_output(format!("unknown predicate '{}'", fact.predicate));
            }
            if fact.evidence.is_empty() {
                return invalid_output(format!(
                    "fact '{}' must include at least one evidence",
                    fact.local_id
                ));
            }
            if let (Some(valid_from), Some(valid_to)) = (fact.valid_from_ms, fact.valid_to_ms) {
                if valid_from > valid_to {
                    return invalid_output(format!(
                        "fact '{}' has valid_from_ms after valid_to_ms",
                        fact.local_id
                    ));
                }
            }
            for evidence in &fact.evidence {
                evidence.validate_against_record(record_text)?;
            }
        }

        Ok(())
    }
}

impl GraphEvidenceSpan {
    fn validate_against_record(&self, record_text: &str) -> MemoryResult<()> {
        match (self.start_byte, self.end_byte) {
            (Some(start), Some(end)) => {
                if start >= end || end > record_text.len() {
                    return invalid_output("evidence byte range is outside record text");
                }
                if !record_text.is_char_boundary(start) || !record_text.is_char_boundary(end) {
                    return invalid_output("evidence byte range is not on UTF-8 boundaries");
                }
                if let Some(expected_text) = &self.text {
                    validate_non_empty("evidence.text", expected_text)?;
                    let actual_text = &record_text[start..end];
                    if actual_text != expected_text {
                        return invalid_output("evidence text does not match record byte range");
                    }
                }
            }
            (None, None) => {
                let Some(expected_text) = &self.text else {
                    return invalid_output("evidence must include text or byte range");
                };
                validate_non_empty("evidence.text", expected_text)?;
                if !record_text.contains(expected_text) {
                    return invalid_output("evidence text is not present in record text");
                }
            }
            _ => {
                return invalid_output("evidence start_byte and end_byte must be set together");
            }
        }
        Ok(())
    }
}

fn validate_non_empty(field: &str, value: &str) -> MemoryResult<()> {
    if value.trim().is_empty() {
        return invalid_output(format!("{field} must not be empty"));
    }
    Ok(())
}

fn validate_confidence(field: &str, confidence: Option<f32>) -> MemoryResult<()> {
    if let Some(confidence) = confidence {
        if !(0.0..=1.0).contains(&confidence) {
            return invalid_output(format!("{field} must be between 0 and 1"));
        }
    }
    Ok(())
}

fn invalid_output<T>(message: impl Into<String>) -> MemoryResult<T> {
    Err(MemoryError::InvalidInput {
        message: format!("INVALID_EXTRACTION_OUTPUT: {}", message.into()),
    })
}

#[async_trait::async_trait]
pub trait GraphExtractor: Send + Sync {
    fn extractor_name(&self) -> &str;
    fn model_name(&self) -> &str;
    fn prompt_version(&self) -> &str;
    fn schema_version(&self) -> &str;
    async fn extract(&self, input: GraphExtractionInput) -> MemoryResult<GraphExtractionOutput>;
}

pub struct GraphExtractionExecutor {
    repository: GraphRepository,
    extractor: Arc<dyn GraphExtractor>,
    type_registry: GraphTypeRegistry,
}

impl GraphExtractionExecutor {
    pub fn new(
        repository: GraphRepository,
        extractor: Arc<dyn GraphExtractor>,
        type_registry: GraphTypeRegistry,
    ) -> Self {
        Self {
            repository,
            extractor,
            type_registry,
        }
    }

    pub async fn process_extraction_stage(
        &self,
        ingestion_run_id: &str,
    ) -> MemoryResult<ExtractionRun> {
        let claim = self
            .repository
            .claim_extraction_run(ingestion_run_id)
            .await?;
        let input = GraphExtractionInput {
            memory_space_id: claim.memory_space_id.clone(),
            memory_record_id: claim.memory_record_id.clone(),
            text: claim.text.clone(),
            metadata: claim.metadata.clone(),
            context_record_ids: claim.context_record_ids.clone(),
            type_registry_version: self.type_registry.version.clone(),
        };

        let started = Instant::now();
        let output = match self.extractor.extract(input).await {
            Ok(output) => output,
            Err(error) => {
                let error_message = error.to_string();
                let _ = self
                    .repository
                    .store_extraction_failure(ExtractionRunFailure {
                        ingestion_run_id: ingestion_run_id.to_string(),
                        memory_space_id: claim.memory_space_id,
                        attempt_count: claim.attempt_count,
                        attempt_number: claim.extraction_attempt_number,
                        extractor_name: self.extractor.extractor_name().to_string(),
                        model: self.extractor.model_name().to_string(),
                        prompt_version: self.extractor.prompt_version().to_string(),
                        schema_version: self.extractor.schema_version().to_string(),
                        type_registry_version: self.type_registry.version.clone(),
                        context_record_ids: claim.context_record_ids,
                        latency_ms: Some(started.elapsed().as_millis() as i64),
                        error_code: "EXTRACTION_FAILED".to_string(),
                        error_message,
                    })
                    .await;
                return Err(error);
            }
        };

        if let Err(error) = output.validate_against_record(&claim.text, &self.type_registry) {
            let error_message = error.to_string();
            let _ = self
                .repository
                .store_extraction_failure(ExtractionRunFailure {
                    ingestion_run_id: ingestion_run_id.to_string(),
                    memory_space_id: claim.memory_space_id,
                    attempt_count: claim.attempt_count,
                    attempt_number: claim.extraction_attempt_number,
                    extractor_name: self.extractor.extractor_name().to_string(),
                    model: self.extractor.model_name().to_string(),
                    prompt_version: self.extractor.prompt_version().to_string(),
                    schema_version: self.extractor.schema_version().to_string(),
                    type_registry_version: self.type_registry.version.clone(),
                    context_record_ids: claim.context_record_ids,
                    latency_ms: Some(started.elapsed().as_millis() as i64),
                    error_code: "INVALID_EXTRACTION_OUTPUT".to_string(),
                    error_message,
                })
                .await;
            return Err(error);
        }

        let result = self
            .repository
            .store_extraction_success(ExtractionRunCompletion {
                ingestion_run_id: ingestion_run_id.to_string(),
                memory_space_id: claim.memory_space_id.clone(),
                attempt_count: claim.attempt_count,
                attempt_number: claim.extraction_attempt_number,
                extractor_name: self.extractor.extractor_name().to_string(),
                model: self.extractor.model_name().to_string(),
                prompt_version: self.extractor.prompt_version().to_string(),
                schema_version: self.extractor.schema_version().to_string(),
                type_registry_version: self.type_registry.version.clone(),
                context_record_ids: claim.context_record_ids.clone(),
                structured_output: serde_json::to_value(&output)?,
                input_tokens: output.input_tokens,
                output_tokens: output.output_tokens,
                latency_ms: Some(started.elapsed().as_millis() as i64),
            })
            .await;

        match result {
            Ok(extraction_run) => Ok(extraction_run),
            Err(error) => {
                let error_message = error.to_string();
                let _ = self
                    .repository
                    .store_extraction_failure(ExtractionRunFailure {
                        ingestion_run_id: ingestion_run_id.to_string(),
                        memory_space_id: claim.memory_space_id,
                        attempt_count: claim.attempt_count,
                        attempt_number: claim.extraction_attempt_number,
                        extractor_name: self.extractor.extractor_name().to_string(),
                        model: self.extractor.model_name().to_string(),
                        prompt_version: self.extractor.prompt_version().to_string(),
                        schema_version: self.extractor.schema_version().to_string(),
                        type_registry_version: self.type_registry.version.clone(),
                        context_record_ids: claim.context_record_ids,
                        latency_ms: Some(started.elapsed().as_millis() as i64),
                        error_code: "EXTRACTION_STORE_FAILED".to_string(),
                        error_message,
                    })
                    .await;
                Err(error)
            }
        }
    }
}

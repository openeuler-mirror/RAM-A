//! Comparison group keys combine fixed time windows with explicit capture and adapter metadata.

use crate::model::corpus::CorpusRecord;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ComparisonGroupKey {
    pub time_window_key: i64,
    pub endpoint_key: String,
    pub model: String,
    pub model_deployment_key: Option<String>,
    pub context_schema_key: String,
    pub agent_key: Option<String>,
    pub kv_namespace: Option<String>,
    pub dialect: String,
    pub adapter_revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComparisonGroupError {
    MissingCapturedAt,
    InvalidCapturedAt(String),
    InvalidWindowSeconds,
    MissingModel,
}

impl ComparisonGroupKey {
    pub fn from_record(
        record: &CorpusRecord,
        window_seconds: u64,
        dialect: &str,
        adapter_revision: &str,
        context_schema_key: &str,
    ) -> Result<Self, ComparisonGroupError> {
        if window_seconds == 0 {
            return Err(ComparisonGroupError::InvalidWindowSeconds);
        }
        let captured_at = record
            .captured_at
            .as_deref()
            .ok_or(ComparisonGroupError::MissingCapturedAt)?;
        let timestamp = time::OffsetDateTime::parse(
            captured_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|error| ComparisonGroupError::InvalidCapturedAt(error.to_string()))?
        .unix_timestamp();
        let window = i64::try_from(window_seconds)
            .map_err(|_| ComparisonGroupError::InvalidWindowSeconds)?;
        let model = record
            .payload
            .get("model")
            .and_then(serde_json::Value::as_str)
            .filter(|model| !model.is_empty())
            .ok_or(ComparisonGroupError::MissingModel)?;
        Ok(Self {
            time_window_key: timestamp.div_euclid(window) * window,
            endpoint_key: record.comparison.endpoint_key.clone(),
            model: model.to_owned(),
            model_deployment_key: record.comparison.model_deployment_key.clone(),
            context_schema_key: context_schema_key.to_owned(),
            agent_key: record.comparison.agent_key.clone(),
            kv_namespace: record.comparison.kv_namespace.clone(),
            dialect: dialect.to_owned(),
            adapter_revision: adapter_revision.to_owned(),
        })
    }
}

#[cfg(test)]
#[path = "../../../../../tests/unit/analyzer/model/comparison/group_key.rs"]
mod tests;

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::error::{PipelineError, Result};
use crate::models::{NormalizedMessage, PipelineIssue};

pub const NORMALIZER_VERSION: &str = "normalize_v1";

pub fn normalize_prepared_memories(
    prepared: &Value,
) -> Result<(Vec<NormalizedMessage>, Vec<PipelineIssue>)> {
    if prepared.get("schema_version").and_then(Value::as_str) != Some("benchmark-prepared-v1") {
        return Err(PipelineError::InvalidInput(
            "prepared input must use benchmark-prepared-v1".into(),
        ));
    }
    let empty_memories = Vec::new();
    let memories = match prepared.get("memories") {
        None => &empty_memories,
        Some(value) => value.as_array().ok_or_else(|| {
            PipelineError::InvalidInput("prepared memories must be a list".into())
        })?,
    };
    let mut messages = Vec::new();
    let mut issues = Vec::new();
    let mut seen = HashSet::new();

    for (source_index, record) in memories.iter().enumerate() {
        let Some(record) = record.as_object() else {
            issues.push(issue(
                "invalid_source_record",
                "source memory must be an object",
                "",
                "",
            ));
            continue;
        };
        let source_id = text(record.get("id")).trim().to_owned();
        if source_id.is_empty() {
            issues.push(issue(
                "missing_source_id",
                "source memory is missing id",
                "",
                "",
            ));
            continue;
        }
        if !seen.insert(source_id.clone()) {
            return Err(PipelineError::InvalidInput(format!(
                "duplicate source message id: {source_id}"
            )));
        }
        let metadata = match record.get("metadata") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(metadata)) => metadata.clone(),
            Some(_) => {
                issues.push(issue(
                    "invalid_source_metadata",
                    "source memory metadata must be an object",
                    &source_id,
                    "",
                ));
                continue;
            }
        };
        let scope_id = text(metadata.get("scope_id")).trim().to_owned();
        if scope_id.is_empty() {
            issues.push(issue(
                "missing_scope_id",
                "source memory is missing metadata.scope_id",
                &source_id,
                "",
            ));
            continue;
        }
        let body = text(record.get("text"));
        if body.trim().is_empty() {
            issues.push(issue(
                "blank_source_message",
                "source memory text is blank",
                &source_id,
                &scope_id,
            ));
            continue;
        }
        let turn_index = metadata
            .get("turn_index")
            .or_else(|| metadata.get("turn_idx"))
            .and_then(optional_int);
        let candidate_eligible = metadata
            .get("memory_candidate")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let timestamp = first_text(&metadata, &["timestamp", "created_at", "session_date"]);
        let role = {
            let value = text(metadata.get("role")).trim().to_owned();
            if value.is_empty() {
                "other".into()
            } else {
                value
            }
        };
        messages.push(NormalizedMessage {
            id: source_id,
            scope_id,
            text: body,
            candidate_eligible,
            role,
            speaker: text(metadata.get("speaker")).trim().to_owned(),
            timestamp,
            session_id: text(metadata.get("session_id")).trim().to_owned(),
            turn_index,
            source_index,
            metadata,
        });
    }
    Ok((messages, issues))
}

fn text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(value)) => value.clone(),
        Some(value) => value.to_string(),
    }
}

fn optional_int(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.parse().ok())
}

fn first_text(metadata: &Map<String, Value>, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| {
            let value = metadata.get(*key)?;
            if value.is_null() || value.as_str() == Some("") {
                None
            } else {
                Some(text(Some(value)).trim().to_owned())
            }
        })
        .unwrap_or_default()
}

fn issue(code: &str, message: &str, source_id: &str, scope_id: &str) -> PipelineIssue {
    PipelineIssue {
        stage: "normalize".into(),
        code: code.into(),
        message: message.into(),
        source_id: source_id.into(),
        scope_id: scope_id.into(),
        ..PipelineIssue::default()
    }
}

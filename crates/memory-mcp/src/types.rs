use std::collections::HashSet;

use anyhow::{bail, Result};
use chrono::DateTime;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const MAX_INGEST_MESSAGES: usize = 100;
pub const MAX_MESSAGE_TEXT_CHARS: usize = 32_000;
pub const MAX_QUERY_CHARS: usize = 32_000;
pub const MAX_TOP_K: usize = 100;
pub const MAX_CASE_TOP_K: usize = 20;

const ALLOWED_ROLES: [&str; 4] = ["user", "assistant", "system", "tool"];
const ALLOWED_MEMORY_TYPES: [&str; 7] = [
    "fact",
    "preference",
    "relationship",
    "event",
    "state",
    "procedure",
    "other",
];

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IngestRequest {
    pub conversation_id: String,
    pub messages: Vec<IngestMessage>,
}

impl IngestRequest {
    pub fn validate(&self) -> Result<()> {
        validate_id("conversation_id", &self.conversation_id)?;
        if self.messages.is_empty() {
            bail!("messages must not be empty");
        }
        if self.messages.len() > MAX_INGEST_MESSAGES {
            bail!("messages must contain at most {MAX_INGEST_MESSAGES} entries");
        }

        let mut message_ids = HashSet::with_capacity(self.messages.len());
        for message in &self.messages {
            validate_id("message id", &message.id)?;
            if !message_ids.insert(message.id.as_str()) {
                bail!("message IDs must be unique");
            }
            if message.text.trim().is_empty() {
                bail!("message text must not be empty");
            }
            if message.text.chars().count() > MAX_MESSAGE_TEXT_CHARS {
                bail!("message text must contain at most {MAX_MESSAGE_TEXT_CHARS} characters");
            }
            if !ALLOWED_ROLES.contains(&message.role.as_str()) {
                bail!("message role is not allowed");
            }
            if let Some(timestamp) = &message.timestamp {
                validate_rfc3339("message timestamp", timestamp)?;
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IngestMessage {
    pub id: String,
    pub role: String,
    pub speaker: Option<String>,
    pub text: String,
    pub timestamp: Option<String>,
    #[serde(default = "default_candidate")]
    pub candidate: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    #[serde(default)]
    pub memory_types: Vec<String>,
    pub event_time_from: Option<String>,
    pub event_time_to: Option<String>,
}

impl SearchRequest {
    pub fn validate(&self) -> Result<()> {
        if self.query.trim().is_empty() {
            bail!("query must not be empty");
        }
        if self.query.chars().count() > MAX_QUERY_CHARS {
            bail!("query must contain at most {MAX_QUERY_CHARS} characters");
        }
        if !(1..=MAX_TOP_K).contains(&self.top_k) {
            bail!("top_k must be between 1 and {MAX_TOP_K}");
        }
        if self
            .memory_types
            .iter()
            .any(|memory_type| !ALLOWED_MEMORY_TYPES.contains(&memory_type.as_str()))
        {
            bail!("memory type is not allowed");
        }
        if let Some(event_time_from) = &self.event_time_from {
            validate_rfc3339("event_time_from", event_time_from)?;
        }
        if let Some(event_time_to) = &self.event_time_to {
            validate_rfc3339("event_time_to", event_time_to)?;
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaseSearchRequest {
    pub query: String,
    #[serde(default)]
    pub library: Option<String>,
    #[serde(default = "default_case_top_k")]
    pub top_k: usize,
}

impl CaseSearchRequest {
    pub fn validate(&self) -> Result<()> {
        if self.query.trim().is_empty() {
            bail!("query must not be empty");
        }
        if self.query.chars().count() > MAX_QUERY_CHARS {
            bail!("query must contain at most {MAX_QUERY_CHARS} characters");
        }
        if !(1..=MAX_CASE_TOP_K).contains(&self.top_k) {
            bail!("top_k must be between 1 and {MAX_CASE_TOP_K}");
        }
        if let Some(library) = &self.library {
            validate_id("library", library)?;
        }
        Ok(())
    }
}

fn default_candidate() -> bool {
    true
}

fn default_top_k() -> usize {
    10
}

fn default_case_top_k() -> usize {
    5
}

fn validate_id(field: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.trim() != value {
        bail!("{field} must be non-empty and must not have surrounding whitespace");
    }
    Ok(())
}

fn validate_rfc3339(field: &str, value: &str) -> Result<()> {
    if DateTime::parse_from_rfc3339(value).is_err() {
        bail!("{field} must be an RFC3339 timestamp");
    }
    Ok(())
}

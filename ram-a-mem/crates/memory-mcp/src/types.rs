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
pub const MAX_CASE_DOCUMENT_CHARS: usize = 512_000;
pub const MAX_CASE_DOCUMENT_ID_CHARS: usize = 255;
pub const MAX_CASE_FILE_NAME_CHARS: usize = 255;
pub const MAX_CASE_DOCUMENT_NAME_CHARS: usize = 512;
pub const MAX_CASE_DIAGNOSIS_CHARS: usize = 8_000;
pub const MAX_CASE_DELETION_REASON_CHARS: usize = 2_000;

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

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaseDocumentUploadRequest {
    #[serde(default)]
    pub library: Option<String>,
    #[serde(default)]
    pub document_id: Option<String>,
    pub file_name: String,
    #[serde(default)]
    pub name: Option<String>,
    pub diagnosis_summary: String,
    pub content: String,
}

impl CaseDocumentUploadRequest {
    pub fn validate(&self) -> Result<()> {
        validate_case_library(self.library.as_deref())?;
        if let Some(document_id) = self.document_id.as_deref() {
            validate_case_document_id(document_id)?;
        }
        validate_case_diagnosis(&self.diagnosis_summary)?;
        validate_case_document(&self.file_name, self.name.as_deref(), &self.content)
    }

    pub(crate) fn mime_type(&self) -> &'static str {
        case_document_mime_type(&self.file_name)
            .expect("validated case document file name has a supported extension")
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaseDocumentUpdateRequest {
    #[serde(default)]
    pub library: Option<String>,
    pub document_id: String,
    pub file_name: String,
    #[serde(default)]
    pub name: Option<String>,
    pub diagnosis_summary: String,
    pub content: String,
}

impl CaseDocumentUpdateRequest {
    pub fn validate(&self) -> Result<()> {
        validate_case_library(self.library.as_deref())?;
        validate_case_document_id(&self.document_id)?;
        validate_case_diagnosis(&self.diagnosis_summary)?;
        validate_case_document(&self.file_name, self.name.as_deref(), &self.content)
    }

    pub(crate) fn mime_type(&self) -> &'static str {
        case_document_mime_type(&self.file_name)
            .expect("validated case document file name has a supported extension")
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaseDocumentDeleteRequest {
    #[serde(default)]
    pub library: Option<String>,
    pub document_id: String,
    pub deletion_reason: String,
}

impl CaseDocumentDeleteRequest {
    pub fn validate(&self) -> Result<()> {
        validate_case_library(self.library.as_deref())?;
        validate_case_document_id(&self.document_id)?;
        if self.deletion_reason.trim().is_empty() {
            bail!("deletion_reason must not be empty");
        }
        if self.deletion_reason.chars().count() > MAX_CASE_DELETION_REASON_CHARS {
            bail!(
                "deletion_reason must contain at most {MAX_CASE_DELETION_REASON_CHARS} characters"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaseMutationConfirmationRequest {
    pub confirmation_token: String,
    pub user_confirmed: bool,
}

impl CaseMutationConfirmationRequest {
    pub fn validate(&self) -> Result<()> {
        validate_id("confirmation_token", &self.confirmation_token)?;
        if self.confirmation_token.len() > 128 {
            bail!("confirmation_token must contain at most 128 bytes");
        }
        if !self.user_confirmed {
            bail!("user_confirmed must be true");
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

fn validate_case_library(library: Option<&str>) -> Result<()> {
    if let Some(library) = library {
        validate_id("library", library)?;
    }
    Ok(())
}

fn validate_case_document_id(document_id: &str) -> Result<()> {
    validate_id("document_id", document_id)?;
    if document_id.chars().count() > MAX_CASE_DOCUMENT_ID_CHARS {
        bail!("document_id must contain at most {MAX_CASE_DOCUMENT_ID_CHARS} characters");
    }
    if document_id == "."
        || document_id == ".."
        || document_id.contains('/')
        || document_id.contains('\\')
    {
        bail!("document_id must be a plain identifier without path components");
    }
    Ok(())
}

fn validate_case_diagnosis(diagnosis_summary: &str) -> Result<()> {
    if diagnosis_summary.trim().is_empty() {
        bail!("diagnosis_summary must not be empty");
    }
    if diagnosis_summary.chars().count() > MAX_CASE_DIAGNOSIS_CHARS {
        bail!("diagnosis_summary must contain at most {MAX_CASE_DIAGNOSIS_CHARS} characters");
    }
    Ok(())
}

fn validate_case_document(file_name: &str, name: Option<&str>, content: &str) -> Result<()> {
    if file_name.is_empty() || file_name.trim() != file_name {
        bail!("file_name must be non-empty and must not have surrounding whitespace");
    }
    if file_name.chars().count() > MAX_CASE_FILE_NAME_CHARS {
        bail!("file_name must contain at most {MAX_CASE_FILE_NAME_CHARS} characters");
    }
    if file_name == "."
        || file_name == ".."
        || file_name.contains('/')
        || file_name.contains('\\')
    {
        bail!("file_name must be a plain file name without path components");
    }
    if case_document_mime_type(file_name).is_none() {
        bail!("file_name must use a supported Markdown or text extension");
    }
    if let Some(name) = name {
        if name.is_empty() || name.trim() != name {
            bail!("name must be non-empty and must not have surrounding whitespace");
        }
        if name.chars().count() > MAX_CASE_DOCUMENT_NAME_CHARS {
            bail!("name must contain at most {MAX_CASE_DOCUMENT_NAME_CHARS} characters");
        }
    }
    if content.trim().is_empty() {
        bail!("content must not be empty");
    }
    if content.chars().count() > MAX_CASE_DOCUMENT_CHARS {
        bail!("content must contain at most {MAX_CASE_DOCUMENT_CHARS} characters");
    }
    Ok(())
}

fn case_document_mime_type(file_name: &str) -> Option<&'static str> {
    let extension = file_name.rsplit_once('.')?.1.to_ascii_lowercase();
    match extension.as_str() {
        "md" | "markdown" | "mdx" => Some("text/markdown"),
        "txt" | "text" | "log" => Some("text/plain"),
        _ => None,
    }
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

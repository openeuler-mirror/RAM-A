use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct PipelineIssue {
    pub stage: String,
    pub code: String,
    pub message: String,
    pub source_id: String,
    pub scope_id: String,
    pub episode_id: String,
    pub window_id: String,
    pub details: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct NormalizedMessage {
    pub id: String,
    pub scope_id: String,
    pub text: String,
    #[serde(
        default = "default_candidate_eligible",
        skip_serializing_if = "candidate_is_eligible"
    )]
    pub candidate_eligible: bool,
    pub role: String,
    pub speaker: String,
    pub timestamp: String,
    pub session_id: String,
    pub turn_index: Option<i64>,
    pub source_index: usize,
    pub metadata: Map<String, Value>,
}

fn default_candidate_eligible() -> bool {
    true
}

fn candidate_is_eligible(value: &bool) -> bool {
    *value
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ConversationEpisode {
    pub id: String,
    pub scope_id: String,
    pub session_id: String,
    pub message_ids: Vec<String>,
    pub start_time: String,
    pub end_time: String,
    pub boundary_reason: String,
    pub episode_version: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MessageRef {
    pub message_id: String,
    pub start_char: usize,
    pub end_char: usize,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExtractionWindow {
    pub id: String,
    pub scope_id: String,
    pub session_id: String,
    pub episode_id: String,
    pub candidate_refs: Vec<MessageRef>,
    pub context_before_refs: Vec<MessageRef>,
    pub context_after_refs: Vec<MessageRef>,
    pub candidate_message_ids: Vec<String>,
    pub candidate_token_count: usize,
    pub total_token_count: usize,
    pub window_version: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EvidenceRef {
    pub message_id: String,
    pub quote: String,
    pub start_char: usize,
    pub end_char: usize,
    pub evidence_role: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AtomicMemory {
    pub id: String,
    pub scope_id: String,
    pub text: String,
    pub memory_type: String,
    pub subject: Map<String, Value>,
    pub predicate: String,
    pub object: Option<Value>,
    pub modality: String,
    pub evidence: Vec<EvidenceRef>,
    pub event_time: Option<Map<String, Value>>,
    pub attributes: Map<String, Value>,
    pub model_confidence: Option<Value>,
    pub observed_at: String,
    pub source_episode_id: String,
    pub source_window_id: String,
    pub observation_refs: Vec<Map<String, Value>>,
}

impl AtomicMemory {
    pub fn canonical_content(&self) -> Value {
        serde_json::json!({
            "memory_type": self.memory_type,
            "text": self.text,
            "subject": self.subject,
            "predicate": self.predicate,
            "object": self.object,
            "modality": self.modality,
            "event_time": self.event_time,
            "attributes": self.attributes,
        })
    }
}

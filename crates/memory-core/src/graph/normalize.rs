use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphInputHashFields {
    pub memory_space_id: String,
    pub session_id: Option<String>,
    pub session_sequence: Option<i64>,
    pub text: String,
    pub source_kind: String,
    pub source_ref: Option<String>,
    pub content_role: String,
    pub observed_at_ms: Option<u64>,
    pub metadata: serde_json::Value,
}

pub fn stable_input_hash(input: &GraphInputHashFields) -> String {
    let mut metadata = input.metadata.clone();
    if let Some(metadata_object) = metadata.as_object_mut() {
        metadata_object.remove("runtime_log");
        metadata_object.remove("trace_id");
    }

    let value = serde_json::json!({
        "memory_space_id": &input.memory_space_id,
        "session_id": &input.session_id,
        "session_sequence": input.session_sequence,
        "text": &input.text,
        "source_kind": &input.source_kind,
        "source_ref": &input.source_ref,
        "content_role": &input.content_role,
        "observed_at_ms": input.observed_at_ms,
        "metadata": metadata,
    });
    let canonical = serde_json::to_string(&value)
        .expect("graph input hash fields must serialize to canonical JSON");
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, canonical.as_bytes()).to_string()
}

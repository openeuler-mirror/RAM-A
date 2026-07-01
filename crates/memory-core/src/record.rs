use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub embedding: Vec<f32>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

use serde::{Deserialize, Serialize};

use crate::record::MemoryRecord;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AddMemoryRequest {
    pub id: Option<String>,
    pub text: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AddMemoryResponse {
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchMemoryRequest {
    pub query: String,
    pub top_k: usize,
    #[serde(default)]
    pub filter: Option<serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Dense,
    Bm25,
    #[default]
    Hybrid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetrievalConfig {
    #[serde(default)]
    pub mode: SearchMode,
    #[serde(default = "default_embedding_weight")]
    pub embedding_weight: f32,
    #[serde(default = "default_bm25_weight")]
    pub bm25_weight: f32,
    #[serde(default)]
    pub candidate_k: Option<usize>,
}

impl RetrievalConfig {
    pub fn candidate_limit(&self, top_k: usize) -> usize {
        self.candidate_k
            .unwrap_or_else(|| top_k.saturating_mul(5).max(100))
    }
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            mode: SearchMode::Hybrid,
            embedding_weight: default_embedding_weight(),
            bm25_weight: default_bm25_weight(),
            candidate_k: None,
        }
    }
}

fn default_embedding_weight() -> f32 {
    0.7
}

fn default_bm25_weight() -> f32 {
    0.3
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScoredMemory {
    pub record: MemoryRecord,
    pub score: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrieval_config_defaults_to_hybrid() {
        let config = RetrievalConfig::default();

        assert_eq!(config.mode, SearchMode::Hybrid);
        assert_eq!(config.embedding_weight, 0.7);
        assert_eq!(config.bm25_weight, 0.3);
        assert_eq!(config.candidate_k, None);
    }
}

use serde::{Deserialize, Serialize};

use crate::graph::{stable_input_hash, GraphInputHashFields};
use crate::record::MemoryRecord;

const GRAPH_SEED_LIMIT_MULTIPLIER: usize = 10;
const MIN_GRAPH_SEED_LIMIT: usize = 30;
const DEFAULT_MAX_EVIDENCE_RECORDS_PER_FACT: usize = 3;

/// Maximum UTF-8 size accepted by graph retrieval. This covers the MCP query
/// contract of 32,000 Unicode scalar values at four bytes each.
pub const MAX_GRAPH_RETRIEVAL_QUERY_BYTES: usize = 32_000 * 4;
/// Hard ceiling for callers using the graph repository directly.
pub const MAX_GRAPH_RETRIEVAL_TOP_K: usize = 500;
/// Hard ceiling for the fact/entity seed pool used by graph expansion.
pub const MAX_GRAPH_SEED_LIMIT: usize = MAX_GRAPH_RETRIEVAL_TOP_K * GRAPH_SEED_LIMIT_MULTIPLIER;
/// Hard ceiling for evidence records loaded for any one graph fact.
pub const MAX_GRAPH_EVIDENCE_RECORDS_PER_FACT: usize = 100;

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
pub struct GraphAddMemoryRequest {
    pub memory_space_id: String,
    pub owner_id: String,
    pub idempotency_key: String,
    pub text: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub session_id: Option<String>,
    pub session_sequence: Option<i64>,
    pub source_kind: String,
    pub source_ref: Option<String>,
    pub content_role: String,
    pub created_by_agent_id: Option<String>,
    pub observed_at_ms: Option<u64>,
}

impl GraphAddMemoryRequest {
    pub fn input_hash(&self) -> String {
        stable_input_hash(&GraphInputHashFields {
            memory_space_id: self.memory_space_id.clone(),
            session_id: self.session_id.clone(),
            session_sequence: self.session_sequence,
            text: self.text.clone(),
            source_kind: self.source_kind.clone(),
            source_ref: self.source_ref.clone(),
            content_role: self.content_role.clone(),
            observed_at_ms: self.observed_at_ms,
            metadata: self.metadata.clone(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphAddMemoryResponse {
    pub memory_record_id: String,
    pub ingestion_run_id: String,
    pub status: String,
    pub vector_ready: bool,
    pub graph_ready: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphRetrieveContextRequest {
    pub memory_space_id: String,
    pub query: String,
    pub top_k: usize,
    pub reference_time_ms: Option<u64>,
    /// Reserved for a future external semantic retrieval provider. The current graph
    /// retrieval path is deterministic and does not consume query embeddings.
    #[serde(default)]
    pub query_embedding: Option<Vec<f32>>,
    #[serde(default)]
    pub query_embedding_model: Option<String>,
    /// Optional application-provided subject override. When absent, graph retrieval
    /// resolves a source actor mentioned by canonical name or alias in the query.
    #[serde(default)]
    pub target_subject_entity_name: Option<String>,
    /// Optional application-provided source-actor override for evidence provenance.
    /// The value resolves through graph entity names and aliases, not record metadata.
    #[serde(default)]
    pub target_evidence_speaker: Option<String>,
    #[serde(default)]
    pub seed_limit: Option<usize>,
    #[serde(default)]
    pub max_evidence_records_per_fact: Option<usize>,
}

impl GraphRetrieveContextRequest {
    pub fn seed_limit(&self) -> usize {
        self.seed_limit.unwrap_or_else(|| {
            self.top_k
                .saturating_mul(GRAPH_SEED_LIMIT_MULTIPLIER)
                .max(MIN_GRAPH_SEED_LIMIT)
        })
    }

    pub fn max_evidence_records_per_fact(&self) -> usize {
        self.max_evidence_records_per_fact
            .unwrap_or(DEFAULT_MAX_EVIDENCE_RECORDS_PER_FACT)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchMemoryRequest {
    pub query: String,
    pub top_k: usize,
    #[serde(default)]
    pub filter: Option<serde_json::Value>,
    #[serde(default)]
    pub graph_memory_space_id: Option<String>,
    #[serde(default)]
    pub graph_target_subject: Option<String>,
    #[serde(default)]
    pub graph_target_evidence_speaker: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Dense,
    Bm25,
    /// Retrieve records only from graph facts and graph-store evidence nodes.
    Graph,
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
    #[serde(default)]
    pub graph: GraphRetrievalConfig,
    #[serde(default)]
    pub rerank: RerankConfig,
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
            graph: GraphRetrievalConfig::default(),
            rerank: RerankConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphRetrievalConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_graph_weight")]
    pub weight: f32,
    #[serde(default)]
    pub rerank_with_graph: bool,
    #[serde(default)]
    pub allow_graph_only: bool,
    #[serde(default)]
    pub seed_limit: Option<usize>,
    #[serde(default)]
    pub max_evidence_records_per_fact: Option<usize>,
    #[serde(default)]
    pub fail_open: bool,
}

impl Default for GraphRetrievalConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            weight: default_graph_weight(),
            rerank_with_graph: false,
            allow_graph_only: false,
            seed_limit: None,
            max_evidence_records_per_fact: None,
            fail_open: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RerankProvider {
    #[default]
    #[serde(rename = "openrouter")]
    OpenRouter,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RerankConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub provider: RerankProvider,
    #[serde(default = "default_rerank_model")]
    pub model: String,
    #[serde(default = "default_rerank_api_key_env")]
    pub api_key_env: String,
    #[serde(default = "default_rerank_base_url")]
    pub base_url: String,
    #[serde(default = "default_rerank_input_k")]
    pub input_k: usize,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub fail_open: bool,
}

impl RerankConfig {
    pub fn input_limit(&self, top_k: usize) -> usize {
        self.input_k.max(top_k)
    }
}

impl Default for RerankConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: RerankProvider::OpenRouter,
            model: default_rerank_model(),
            api_key_env: default_rerank_api_key_env(),
            base_url: default_rerank_base_url(),
            input_k: default_rerank_input_k(),
            timeout_ms: None,
            fail_open: false,
        }
    }
}

fn default_embedding_weight() -> f32 {
    0.7
}

fn default_bm25_weight() -> f32 {
    0.3
}

fn default_graph_weight() -> f32 {
    0.2
}

fn default_rerank_model() -> String {
    "cohere/rerank-v3.5".to_string()
}

fn default_rerank_api_key_env() -> String {
    "OPENROUTER_API_KEY".to_string()
}

fn default_rerank_base_url() -> String {
    "https://openrouter.ai/api/v1".to_string()
}

fn default_rerank_input_k() -> usize {
    40
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
        assert!(!config.graph.enabled);
        assert_eq!(config.graph.weight, 0.2);
        assert_eq!(config.graph.seed_limit, None);
        assert_eq!(config.graph.max_evidence_records_per_fact, None);
        assert!(!config.graph.fail_open);
        assert!(!config.rerank.enabled);
        assert_eq!(config.rerank.provider, RerankProvider::OpenRouter);
        assert_eq!(config.rerank.model, "cohere/rerank-v3.5");
        assert_eq!(config.rerank.api_key_env, "OPENROUTER_API_KEY");
        assert_eq!(config.rerank.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(config.rerank.input_k, 40);
        assert_eq!(config.rerank.timeout_ms, None);
        assert!(!config.rerank.fail_open);
    }

    #[test]
    fn rerank_input_limit_keeps_enough_candidates_for_final_top_k() {
        let mut config = RerankConfig {
            enabled: true,
            input_k: 40,
            ..RerankConfig::default()
        };

        assert_eq!(config.input_limit(5), 40);
        assert_eq!(config.input_limit(40), 40);
        assert_eq!(config.input_limit(50), 50);

        config.input_k = 0;
        assert_eq!(config.input_limit(5), 5);
    }

    #[test]
    fn rerank_provider_serializes_as_openrouter() {
        let value = serde_json::to_value(RerankProvider::OpenRouter).expect("serialize provider");
        assert_eq!(value, serde_json::json!("openrouter"));

        let provider: RerankProvider =
            serde_json::from_str("\"openrouter\"").expect("deserialize provider");
        assert_eq!(provider, RerankProvider::OpenRouter);
    }

    #[test]
    fn graph_retrieve_context_request_defaults_are_bounded() {
        let request = GraphRetrieveContextRequest {
            memory_space_id: "space-1".to_string(),
            query: "Where does Alice live?".to_string(),
            top_k: 3,
            reference_time_ms: None,
            query_embedding: None,
            query_embedding_model: None,
            target_subject_entity_name: None,
            target_evidence_speaker: None,
            seed_limit: None,
            max_evidence_records_per_fact: None,
        };

        assert_eq!(request.seed_limit(), 30);
        assert_eq!(request.max_evidence_records_per_fact(), 3);
    }
}

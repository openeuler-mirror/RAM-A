use std::collections::HashSet;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub auth: AuthConfig,
    #[serde(default)]
    pub features: FeaturesConfig,
    #[serde(default)]
    pub http: HttpConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub pipeline: PipelineServiceConfig,
    #[serde(default)]
    pub storage: Option<StorageConfig>,
    #[serde(default)]
    pub providers: Option<ProvidersConfig>,
    #[serde(default)]
    pub retrieval: RetrievalServiceConfig,
    #[serde(default)]
    pub case_library: Option<CaseLibraryServiceConfig>,
    #[serde(default)]
    pub graph_memory: Option<GraphMemoryServiceConfig>,
}

pub const MAX_MCP_BODY_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_REQUESTS_PER_SECOND: u32 = 10_000;
pub const MAX_RATE_BURST: u32 = 100_000;
pub const MAX_IN_FLIGHT_PER_PRINCIPAL_TOOL: usize = 1_024;
pub const MAX_INITIALIZE_REQUESTS_PER_SECOND: u32 = 1_000;
pub const MAX_INITIALIZE_RATE_BURST: u32 = 10_000;
pub const MAX_ACTIVE_SESSIONS_PER_PRINCIPAL: usize = 1_024;
pub const MAX_ACTIVE_SESSIONS_GLOBAL: usize = 100_000;
pub const MAX_SESSION_IDLE_TIMEOUT_SECONDS: u64 = 86_400;
pub const DEFAULT_RERANK_TIMEOUT_MS: u64 = 30_000;
pub const MAX_RERANK_TIMEOUT_MS: u64 = 120_000;
const SUPPORTED_PERMISSIONS: [&str; 4] =
    ["memory:read", "memory:write", "cases:read", "cases:write"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeatureFlags {
    pub memory: bool,
    pub case_library: bool,
}

impl FeatureFlags {
    pub fn all() -> Self {
        Self {
            memory: true,
            case_library: true,
        }
    }
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self::all()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct FeaturesConfig {
    pub memory: MemoryFeatureConfig,
    pub case_library: CaseLibraryFeatureConfig,
    pub graph_memory: GraphMemoryFeatureConfig,
}

impl FeaturesConfig {
    pub fn resolve(&self, case_library_configured: bool) -> FeatureFlags {
        FeatureFlags {
            memory: self.memory.enabled,
            case_library: self.case_library.enabled.unwrap_or(case_library_configured),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryFeatureConfig {
    pub enabled: bool,
}

impl Default for MemoryFeatureConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaseLibraryFeatureConfig {
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphMemoryFeatureConfig {
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphMemoryServiceConfig {
    pub llm_api_key_env: String,
    #[serde(default = "default_provider_base_url")]
    pub llm_base_url: String,
    pub llm_model: String,
    #[serde(default = "default_graph_llm_timeout_ms")]
    pub llm_timeout_ms: u64,
    #[serde(default = "default_graph_build_concurrency")]
    pub build_concurrency: usize,
    #[serde(default)]
    pub retrieval: GraphMemoryRetrievalConfig,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphMemoryRetrievalConfig {
    pub weight: f32,
    pub rerank_with_graph: bool,
    pub allow_graph_only: bool,
    pub max_graph_only_results: Option<usize>,
    pub seed_limit: Option<usize>,
    pub max_evidence_records_per_fact: Option<usize>,
    pub fail_open: bool,
}

impl Default for GraphMemoryRetrievalConfig {
    fn default() -> Self {
        let defaults = memory_core::GraphRetrievalConfig::default();
        Self {
            weight: defaults.weight,
            rerank_with_graph: defaults.rerank_with_graph,
            allow_graph_only: defaults.allow_graph_only,
            max_graph_only_results: defaults.max_graph_only_results,
            seed_limit: defaults.seed_limit,
            max_evidence_records_per_fact: defaults.max_evidence_records_per_fact,
            fail_open: defaults.fail_open,
        }
    }
}

impl GraphMemoryRetrievalConfig {
    pub fn core_config(&self) -> memory_core::GraphRetrievalConfig {
        memory_core::GraphRetrievalConfig {
            enabled: true,
            weight: self.weight,
            rerank_with_graph: self.rerank_with_graph,
            allow_graph_only: self.allow_graph_only,
            max_graph_only_results: self.max_graph_only_results,
            seed_limit: self.seed_limit,
            max_evidence_records_per_fact: self.max_evidence_records_per_fact,
            fail_open: self.fail_open,
        }
    }
}

impl GraphMemoryServiceConfig {
    fn validate(&self) -> Result<()> {
        if self.llm_api_key_env.trim().is_empty() || self.llm_model.trim().is_empty() {
            anyhow::bail!("graph memory LLM configuration is incomplete");
        }
        validate_authenticated_provider_base_url(
            &self.llm_base_url,
            "graph memory LLM base URL",
            Some(&self.llm_api_key_env),
        )?;
        if self.llm_timeout_ms == 0 || self.build_concurrency == 0 {
            anyhow::bail!("graph memory timeout and build concurrency must be non-zero");
        }
        if !self.retrieval.weight.is_finite() || !(0.0..=1.0).contains(&self.retrieval.weight) {
            anyhow::bail!("graph memory retrieval weight must be between 0 and 1");
        }
        if self.retrieval.max_graph_only_results == Some(0)
            || self.retrieval.seed_limit == Some(0)
            || self.retrieval.max_evidence_records_per_fact == Some(0)
        {
            anyhow::bail!("graph memory retrieval limits must be non-zero when configured");
        }
        if self
            .retrieval
            .seed_limit
            .is_some_and(|limit| limit > memory_core::MAX_GRAPH_SEED_LIMIT)
        {
            anyhow::bail!(
                "graph memory retrieval seed_limit must not exceed {}",
                memory_core::MAX_GRAPH_SEED_LIMIT
            );
        }
        if self
            .retrieval
            .max_evidence_records_per_fact
            .is_some_and(|limit| limit > memory_core::MAX_GRAPH_EVIDENCE_RECORDS_PER_FACT)
        {
            anyhow::bail!(
                "graph memory retrieval max_evidence_records_per_fact must not exceed {}",
                memory_core::MAX_GRAPH_EVIDENCE_RECORDS_PER_FACT
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    #[serde(default = "default_bind_address")]
    pub bind_address: IpAddr,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    #[serde(default = "default_allowed_hosts")]
    pub allowed_hosts: Vec<String>,
    #[serde(default)]
    pub tls_termination_acknowledged: bool,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind_address: default_bind_address(),
            port: default_port(),
            allowed_origins: Vec::new(),
            allowed_hosts: default_allowed_hosts(),
            tls_termination_acknowledged: false,
        }
    }
}

impl HttpConfig {
    pub fn socket_address(&self) -> std::net::SocketAddr {
        (self.bind_address, self.port).into()
    }

    pub fn validate_bind(&self) -> Result<()> {
        if self.allowed_hosts.is_empty()
            || self.allowed_hosts.iter().any(|host| host.trim().is_empty())
        {
            anyhow::bail!("HTTP allowed hosts must be explicitly configured");
        }
        if !self.bind_address.is_loopback() && !self.tls_termination_acknowledged {
            anyhow::bail!("external bind requires explicit TLS termination acknowledgement");
        }
        if !self.bind_address.is_loopback()
            && !self
                .allowed_hosts
                .iter()
                .any(|host| !is_loopback_host(host))
        {
            anyhow::bail!("external bind requires an external allowed host");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsConfig {
    #[serde(default = "default_max_body_bytes")]
    pub max_body_bytes: usize,
    #[serde(default = "default_requests_per_second")]
    pub requests_per_second: u32,
    #[serde(default = "default_rate_burst")]
    pub rate_burst: u32,
    #[serde(default = "default_max_in_flight")]
    pub max_in_flight_per_principal_tool: usize,
    #[serde(default = "default_initialize_requests_per_second")]
    pub initialize_requests_per_second: u32,
    #[serde(default = "default_initialize_rate_burst")]
    pub initialize_rate_burst: u32,
    #[serde(default = "default_max_active_sessions_per_principal")]
    pub max_active_sessions_per_principal: usize,
    #[serde(default = "default_max_active_sessions_global")]
    pub max_active_sessions_global: usize,
    #[serde(default = "default_session_idle_timeout_seconds")]
    pub session_idle_timeout_seconds: u64,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_body_bytes: default_max_body_bytes(),
            requests_per_second: default_requests_per_second(),
            rate_burst: default_rate_burst(),
            max_in_flight_per_principal_tool: default_max_in_flight(),
            initialize_requests_per_second: default_initialize_requests_per_second(),
            initialize_rate_burst: default_initialize_rate_burst(),
            max_active_sessions_per_principal: default_max_active_sessions_per_principal(),
            max_active_sessions_global: default_max_active_sessions_global(),
            session_idle_timeout_seconds: default_session_idle_timeout_seconds(),
        }
    }
}

impl LimitsConfig {
    fn validate(&self) -> Result<()> {
        let within_bounds = self.max_body_bytes <= MAX_MCP_BODY_BYTES
            && self.requests_per_second <= MAX_REQUESTS_PER_SECOND
            && self.rate_burst <= MAX_RATE_BURST
            && self.max_in_flight_per_principal_tool <= MAX_IN_FLIGHT_PER_PRINCIPAL_TOOL
            && self.initialize_requests_per_second <= MAX_INITIALIZE_REQUESTS_PER_SECOND
            && self.initialize_rate_burst <= MAX_INITIALIZE_RATE_BURST
            && self.max_active_sessions_per_principal <= MAX_ACTIVE_SESSIONS_PER_PRINCIPAL
            && self.max_active_sessions_global <= MAX_ACTIVE_SESSIONS_GLOBAL
            && self.session_idle_timeout_seconds <= MAX_SESSION_IDLE_TIMEOUT_SECONDS;
        let all_nonzero = self.max_body_bytes > 0
            && self.requests_per_second > 0
            && self.rate_burst > 0
            && self.max_in_flight_per_principal_tool > 0
            && self.initialize_requests_per_second > 0
            && self.initialize_rate_burst > 0
            && self.max_active_sessions_per_principal > 0
            && self.max_active_sessions_global > 0
            && self.session_idle_timeout_seconds > 0;
        if !all_nonzero || !within_bounds {
            anyhow::bail!("HTTP limits are outside the supported range");
        }
        if self.max_active_sessions_global < self.max_active_sessions_per_principal {
            anyhow::bail!("global active session limit must be at least the per-principal limit");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PipelineServiceConfig {
    pub fail_fast: bool,
    pub max_memory_chars: usize,
    pub max_candidate_tokens: usize,
    pub max_window_tokens: usize,
    pub extractor_max_output_tokens: usize,
    pub verifier_max_output_tokens: usize,
    pub extractor_context_window_tokens: Option<usize>,
    pub verifier_context_window_tokens: Option<usize>,
    pub reasoning_reserve_tokens: usize,
}

impl Default for PipelineServiceConfig {
    fn default() -> Self {
        let defaults = memory_pipeline::pipeline::PipelineConfig::default();
        Self {
            fail_fast: defaults.fail_fast,
            max_memory_chars: defaults.validation.max_memory_chars,
            max_candidate_tokens: defaults.window.max_candidate_tokens,
            max_window_tokens: defaults.window.max_window_tokens,
            extractor_max_output_tokens: 1600,
            verifier_max_output_tokens: 1000,
            extractor_context_window_tokens: None,
            verifier_context_window_tokens: None,
            reasoning_reserve_tokens: 0,
        }
    }
}

impl PipelineServiceConfig {
    pub fn pipeline_config(&self) -> memory_pipeline::pipeline::PipelineConfig {
        let mut config = memory_pipeline::pipeline::PipelineConfig::default();
        config.fail_fast = self.fail_fast;
        config.validation.max_memory_chars = self.max_memory_chars;
        config.window.max_candidate_tokens = self.max_candidate_tokens;
        config.window.max_window_tokens = self.max_window_tokens;
        config
    }

    fn validate(&self) -> Result<()> {
        if !(1..=crate::MAX_MESSAGE_TEXT_CHARS).contains(&self.max_memory_chars) {
            anyhow::bail!("pipeline max_memory_chars must be between 1 and 32000");
        }
        if self.max_candidate_tokens == 0 || self.max_window_tokens < self.max_candidate_tokens {
            anyhow::bail!(
                "pipeline token windows require 0 < max_candidate_tokens <= max_window_tokens"
            );
        }
        if self.extractor_max_output_tokens == 0 || self.verifier_max_output_tokens == 0 {
            anyhow::bail!("pipeline model output token budgets must be positive");
        }
        for (name, context, output) in [
            (
                "extractor",
                self.extractor_context_window_tokens,
                self.extractor_max_output_tokens,
            ),
            (
                "verifier",
                self.verifier_context_window_tokens,
                self.verifier_max_output_tokens,
            ),
        ] {
            if context.is_some_and(|value| value <= output + self.reasoning_reserve_tokens) {
                anyhow::bail!(
                    "pipeline {name} context window must exceed output and reasoning reserve"
                );
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    pub database_path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaseLibraryServiceConfig {
    #[serde(default = "default_case_rag_store")]
    pub rag_store: PathBuf,
    #[serde(default = "default_case_index_store")]
    pub index_store: PathBuf,
    #[serde(default)]
    pub source_dir: Option<PathBuf>,
    /// Enables the case-management REST API when configured. The named
    /// environment variable supplies its dedicated administrator bearer token.
    #[serde(default)]
    pub api_token_env: Option<String>,
    #[serde(default = "default_case_ingestion_poll_ms")]
    pub ingestion_poll_ms: u64,
    #[serde(default)]
    pub embedding_provider: EmbeddingProviderKind,
    #[serde(default)]
    pub embedding_api_key_env: Option<String>,
    #[serde(default)]
    pub embedding_base_url: Option<String>,
    #[serde(default = "default_case_embedding_model")]
    pub embedding_model: String,
    #[serde(default = "default_case_embedding_dimensions")]
    pub embedding_dimensions: usize,
    #[serde(default = "default_case_chunk_size")]
    pub chunk_size: usize,
    #[serde(default)]
    pub summary_llm_model: Option<String>,
    #[serde(default)]
    pub summary_llm_api_key_env: Option<String>,
    #[serde(default)]
    pub summary_llm_base_url: Option<String>,
    #[serde(default = "default_summary_llm_timeout_ms")]
    pub summary_llm_timeout_ms: u64,
    pub default_library: String,
    pub libraries: Vec<CaseLibraryConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProvidersConfig {
    pub api_key_env: String,
    #[serde(default = "default_provider_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub enable_thinking: Option<bool>,
    #[serde(default = "default_true")]
    pub send_temperature: bool,
    #[serde(default)]
    pub temperature: f64,
    #[serde(default)]
    pub output_token_parameter: memory_pipeline::client::OutputTokenParameter,
    #[serde(default)]
    pub structured_output: memory_pipeline::client::StructuredOutputMode,
    #[serde(default)]
    pub reasoning_only_retry: bool,
    #[serde(default)]
    pub json_repair_attempts: usize,
    #[serde(default)]
    pub embedding_provider: EmbeddingProviderKind,
    #[serde(default)]
    pub embedding_api_key_env: Option<String>,
    #[serde(default)]
    pub embedding_base_url: Option<String>,
    pub embedding_model: String,
    pub embedding_dimensions: usize,
    pub extractor_model: String,
    pub verifier_model: String,
    #[serde(default = "default_provider_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_provider_max_retries")]
    pub max_retries: usize,
}

impl ProvidersConfig {
    pub fn chat_compatibility(&self) -> memory_pipeline::client::ChatCompatibilityOptions {
        memory_pipeline::client::ChatCompatibilityOptions {
            reasoning_effort: self.reasoning_effort.clone(),
            enable_thinking: self.enable_thinking,
            send_temperature: self.send_temperature,
            temperature: self.temperature,
            output_token_parameter: self.output_token_parameter,
            structured_output: self.structured_output,
            reasoning_only_retry: self.reasoning_only_retry,
            json_repair_attempts: self.json_repair_attempts,
        }
    }

    pub fn resolved_embedding_api_key_env(&self) -> &str {
        self.embedding_api_key_env
            .as_deref()
            .unwrap_or(&self.api_key_env)
    }

    pub fn resolved_embedding_base_url(&self) -> &str {
        self.embedding_base_url.as_deref().unwrap_or(&self.base_url)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetrievalServiceConfig {
    pub mode: memory_core::SearchMode,
    pub embedding_weight: f32,
    pub bm25_weight: f32,
    pub candidate_k: Option<usize>,
    pub rerank: RerankServiceConfig,
}

impl Default for RetrievalServiceConfig {
    fn default() -> Self {
        let defaults = memory_core::RetrievalConfig::default();
        Self {
            mode: defaults.mode,
            embedding_weight: defaults.embedding_weight,
            bm25_weight: defaults.bm25_weight,
            candidate_k: defaults.candidate_k,
            rerank: RerankServiceConfig::default(),
        }
    }
}

impl RetrievalServiceConfig {
    pub fn core_config(
        &self,
        graph: memory_core::GraphRetrievalConfig,
    ) -> memory_core::RetrievalConfig {
        memory_core::RetrievalConfig {
            mode: self.mode,
            embedding_weight: self.embedding_weight,
            bm25_weight: self.bm25_weight,
            candidate_k: self.candidate_k,
            graph,
            rerank: self.rerank.core_config(),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.mode == memory_core::SearchMode::Graph {
            anyhow::bail!(
                "retrieval mode graph is not supported by the MCP service; enable graph_memory augmentation instead"
            );
        }
        if self
            .candidate_k
            .is_some_and(|value| !(1..=500).contains(&value))
        {
            anyhow::bail!("retrieval candidate_k must be between 1 and 500");
        }
        if self.mode == memory_core::SearchMode::Hybrid {
            let weights = [self.embedding_weight, self.bm25_weight];
            if weights
                .iter()
                .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
                || (weights.iter().sum::<f32>() - 1.0).abs() > 1e-6
            {
                anyhow::bail!(
                    "hybrid retrieval weights must be finite, between 0 and 1, and sum to 1"
                );
            }
        }
        self.rerank.validate(self.mode)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RerankServiceConfig {
    pub enabled: bool,
    pub provider: memory_core::RerankProvider,
    pub model: String,
    pub api_key_env: Option<String>,
    pub base_url: String,
    pub input_k: usize,
    pub timeout_ms: Option<u64>,
    pub fail_open: bool,
}

impl Default for RerankServiceConfig {
    fn default() -> Self {
        let defaults = memory_core::RerankConfig::default();
        Self {
            enabled: defaults.enabled,
            provider: defaults.provider,
            model: defaults.model,
            api_key_env: Some(defaults.api_key_env),
            base_url: defaults.base_url,
            input_k: defaults.input_k,
            timeout_ms: Some(DEFAULT_RERANK_TIMEOUT_MS),
            fail_open: defaults.fail_open,
        }
    }
}

impl RerankServiceConfig {
    pub fn core_config(&self) -> memory_core::RerankConfig {
        memory_core::RerankConfig {
            enabled: self.enabled,
            provider: self.provider,
            model: self.model.clone(),
            api_key_env: self.api_key_env.clone().unwrap_or_default(),
            base_url: self.base_url.clone(),
            input_k: self.input_k,
            timeout_ms: self.timeout_ms,
            fail_open: self.fail_open,
        }
    }

    fn validate(&self, mode: memory_core::SearchMode) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        if mode != memory_core::SearchMode::Hybrid {
            anyhow::bail!("rerank requires retrieval mode hybrid");
        }
        if self.model.trim().is_empty() || self.base_url.trim().is_empty() {
            anyhow::bail!("rerank model and base_url must not be empty");
        }
        if self
            .api_key_env
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            anyhow::bail!("rerank API key environment name must not be empty");
        }
        validate_authenticated_provider_base_url(
            &self.base_url,
            "rerank base URL",
            self.api_key_env.as_deref(),
        )?;
        if !(1..=500).contains(&self.input_k) {
            anyhow::bail!("rerank input_k must be between 1 and 500");
        }
        if !matches!(self.timeout_ms, Some(1..=MAX_RERANK_TIMEOUT_MS)) {
            anyhow::bail!(
                "enabled rerank timeout_ms must be between 1 and {MAX_RERANK_TIMEOUT_MS}"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum EmbeddingProviderKind {
    #[default]
    #[serde(rename = "openai_compatible", alias = "open_router")]
    OpenAiCompatible,
    #[serde(rename = "hash")]
    Hash,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaseServiceConfig {
    pub base_url: String,
    pub bearer_token_env: String,
    #[serde(default = "default_case_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_case_max_response_bytes")]
    pub max_response_bytes: usize,
    pub default_library: String,
    pub libraries: Vec<CaseLibraryConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaseLibraryConfig {
    pub name: String,
    pub dataset_id: String,
    pub tenant_ids: Vec<String>,
}

impl CaseServiceConfig {
    pub fn validate(&self) -> Result<()> {
        let base_url =
            url::Url::parse(&self.base_url).context("case service base URL is not valid")?;
        if !matches!(base_url.scheme(), "http" | "https") || base_url.host_str().is_none() {
            anyhow::bail!("case service base URL must be an absolute HTTP or HTTPS URL");
        }
        if !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            anyhow::bail!("case service base URL must not contain credentials, query, or fragment");
        }
        if self.bearer_token_env.trim().is_empty() {
            anyhow::bail!("case service bearer token environment name must not be empty");
        }
        if self.timeout_seconds == 0 || self.max_response_bytes == 0 {
            anyhow::bail!("case service limits must be non-zero");
        }
        if self.default_library.trim().is_empty() || self.libraries.is_empty() {
            anyhow::bail!("case service requires a default library and library mappings");
        }

        let mut names = HashSet::with_capacity(self.libraries.len());
        for library in &self.libraries {
            if library.name.trim().is_empty()
                || library.name.trim() != library.name
                || library.dataset_id.trim().is_empty()
                || library.dataset_id.trim() != library.dataset_id
                || library.tenant_ids.is_empty()
                || library
                    .tenant_ids
                    .iter()
                    .any(|tenant| tenant.trim().is_empty() || tenant.trim() != tenant)
            {
                anyhow::bail!("case library mappings must use canonical non-empty values");
            }
            if !names.insert(library.name.as_str()) {
                anyhow::bail!("case library names must be unique");
            }
        }
        if !names.contains(self.default_library.as_str()) {
            anyhow::bail!("default case library must reference a configured library");
        }
        Ok(())
    }
}

impl CaseLibraryServiceConfig {
    pub fn resolved_embedding_api_key_env<'a>(&'a self, providers: &'a ProvidersConfig) -> &'a str {
        self.embedding_api_key_env
            .as_deref()
            .unwrap_or(&providers.api_key_env)
    }

    pub fn resolved_embedding_base_url<'a>(&'a self, providers: &'a ProvidersConfig) -> &'a str {
        self.embedding_base_url
            .as_deref()
            .unwrap_or(&providers.base_url)
    }

    pub fn resolved_summary_api_key_env<'a>(&'a self, providers: &'a ProvidersConfig) -> &'a str {
        self.summary_llm_api_key_env
            .as_deref()
            .unwrap_or(&providers.api_key_env)
    }

    pub fn resolved_summary_base_url<'a>(&'a self, providers: &'a ProvidersConfig) -> &'a str {
        self.summary_llm_base_url
            .as_deref()
            .unwrap_or(&providers.base_url)
    }

    pub fn validate(&self, memory_database_path: Option<&Path>) -> Result<()> {
        validate_case_library_mappings(self.default_library.as_str(), &self.libraries)?;
        if self.rag_store.as_os_str().is_empty()
            || self.index_store.as_os_str().is_empty()
            || self.rag_store == Path::new(":memory:")
            || self.index_store == Path::new(":memory:")
        {
            anyhow::bail!("case library stores must use persistent SQLite files");
        }
        if self.rag_store == self.index_store {
            anyhow::bail!("case library rag_store and index_store must be different files");
        }
        if memory_database_path.is_some_and(|path| path == self.index_store) {
            anyhow::bail!("case library index_store must be separate from RAM-A memory storage");
        }
        if self.embedding_model.trim().is_empty()
            || self.embedding_dimensions == 0
            || self.chunk_size == 0
            || self.summary_llm_timeout_ms == 0
        {
            anyhow::bail!("case library provider configuration is incomplete");
        }
        if let Some(source_dir) = &self.source_dir {
            if source_dir.as_os_str().is_empty() {
                anyhow::bail!("case library source_dir must not be empty");
            }
        }
        if let Some(api_token_env) = self.api_token_env.as_deref() {
            if api_token_env.trim().is_empty() || api_token_env.trim() != api_token_env {
                anyhow::bail!(
                    "case library API token environment name must be canonical and non-empty"
                );
            }
        }
        if self.ingestion_poll_ms == 0 {
            anyhow::bail!("case library ingestion_poll_ms must be non-zero");
        }
        if let Some(embedding_api_key_env) = self.embedding_api_key_env.as_deref() {
            if embedding_api_key_env.trim().is_empty() {
                anyhow::bail!("case library embedding API key environment name must not be empty");
            }
        }
        if let Some(embedding_base_url) = self.embedding_base_url.as_deref() {
            validate_provider_base_url(embedding_base_url, "case library embedding base URL")?;
        }
        if let Some(summary_api_key_env) = self.summary_llm_api_key_env.as_deref() {
            if summary_api_key_env.trim().is_empty() {
                anyhow::bail!(
                    "case library summary LLM API key environment name must not be empty"
                );
            }
        }
        if let Some(summary_base_url) = self.summary_llm_base_url.as_deref() {
            validate_provider_base_url(summary_base_url, "case library summary LLM base URL")?;
        }
        if self
            .summary_llm_model
            .as_deref()
            .is_some_and(|model| model.trim().is_empty())
        {
            anyhow::bail!("case library summary LLM model must not be empty when configured");
        }
        Ok(())
    }
}

fn default_bind_address() -> IpAddr {
    IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

fn default_port() -> u16 {
    8080
}

fn default_allowed_hosts() -> Vec<String> {
    vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ]
}

fn is_loopback_host(value: &str) -> bool {
    let value = value.trim();
    let host = value
        .parse::<axum::http::uri::Authority>()
        .map(|authority| authority.host().to_string())
        .unwrap_or_else(|_| value.to_string());
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn default_max_body_bytes() -> usize {
    16 * 1024 * 1024
}

fn default_requests_per_second() -> u32 {
    20
}

fn default_rate_burst() -> u32 {
    40
}

fn default_max_in_flight() -> usize {
    4
}

fn default_initialize_requests_per_second() -> u32 {
    4
}

fn default_initialize_rate_burst() -> u32 {
    8
}

fn default_max_active_sessions_per_principal() -> usize {
    8
}

fn default_max_active_sessions_global() -> usize {
    256
}

fn default_session_idle_timeout_seconds() -> u64 {
    1_800
}

fn default_provider_base_url() -> String {
    "https://openrouter.ai/api/v1".to_string()
}

fn default_provider_timeout_seconds() -> u64 {
    120
}

fn default_provider_max_retries() -> usize {
    3
}

fn default_true() -> bool {
    true
}

fn default_case_timeout_seconds() -> u64 {
    5
}

fn default_case_max_response_bytes() -> usize {
    262_144
}

fn default_case_rag_store() -> PathBuf {
    PathBuf::from("data/memory-cases.sqlite")
}

fn default_case_index_store() -> PathBuf {
    PathBuf::from("data/memory-cases-index.sqlite")
}

fn default_case_embedding_model() -> String {
    "hash".to_string()
}

fn default_case_embedding_dimensions() -> usize {
    1_024
}

fn default_case_chunk_size() -> usize {
    160
}

fn default_case_ingestion_poll_ms() -> u64 {
    1_000
}

fn default_summary_llm_timeout_ms() -> u64 {
    30_000
}

fn default_graph_llm_timeout_ms() -> u64 {
    60_000
}

fn default_graph_build_concurrency() -> usize {
    1
}

impl ServerConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read server config `{}`", path.display()))?;
        serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse server config `{}`", path.display()))
    }

    pub fn validate_runtime(&self) -> Result<()> {
        self.http.validate_bind()?;
        self.limits.validate()?;
        self.pipeline.validate()?;
        self.retrieval.validate()?;
        self.auth.validate()?;
        if self.features.case_library.enabled == Some(true) && self.case_library.is_none() {
            anyhow::bail!("case_library feature requires case_library configuration");
        }
        if self.features.graph_memory.enabled && self.graph_memory.is_none() {
            anyhow::bail!("graph_memory feature requires graph_memory configuration");
        }
        if self.features.graph_memory.enabled && !self.features.memory.enabled {
            anyhow::bail!("graph_memory feature requires the memory feature");
        }
        let storage = self
            .storage
            .as_ref()
            .context("production runtime requires storage configuration")?;
        if storage.database_path.as_os_str().is_empty()
            || storage.database_path == Path::new(":memory:")
        {
            anyhow::bail!("production runtime requires a persistent SQLite file");
        }
        if let Some(case_library) = &self.case_library {
            case_library.validate(Some(&storage.database_path))?;
        }
        if let Some(graph_memory) = &self.graph_memory {
            graph_memory.validate()?;
        }
        let providers = self
            .providers
            .as_ref()
            .context("production runtime requires provider configuration")?;
        if [
            providers.api_key_env.as_str(),
            providers.base_url.as_str(),
            providers.embedding_model.as_str(),
            providers.extractor_model.as_str(),
            providers.verifier_model.as_str(),
        ]
        .iter()
        .any(|value| value.trim().is_empty())
            || providers.embedding_dimensions == 0
            || providers.timeout_seconds == 0
            || providers.max_retries == 0
        {
            anyhow::bail!("provider configuration is incomplete");
        }
        validate_authenticated_provider_base_url(
            &providers.base_url,
            "provider base URL",
            Some(&providers.api_key_env),
        )?;
        if let Some(embedding_api_key_env) = providers.embedding_api_key_env.as_deref() {
            if embedding_api_key_env.trim().is_empty() {
                anyhow::bail!("embedding API key environment name must not be empty");
            }
        }
        if let Some(reasoning_effort) = providers.reasoning_effort.as_deref() {
            if reasoning_effort.trim().is_empty() || reasoning_effort.trim() != reasoning_effort {
                anyhow::bail!("provider reasoning effort must be canonical and non-empty");
            }
        }
        if providers.reasoning_effort.is_some() && providers.enable_thinking.is_some() {
            anyhow::bail!("configure at most one of provider reasoning_effort and enable_thinking");
        }
        if !providers.temperature.is_finite() || !(-2.0..=2.0).contains(&providers.temperature) {
            anyhow::bail!("provider temperature must be finite and between -2 and 2");
        }
        if providers.json_repair_attempts > 1 {
            anyhow::bail!("provider json_repair_attempts must be 0 or 1");
        }
        if let Some(embedding_base_url) = providers.embedding_base_url.as_deref() {
            validate_provider_base_url(embedding_base_url, "embedding base URL")?;
        }
        if providers.embedding_provider == EmbeddingProviderKind::OpenAiCompatible {
            validate_authenticated_provider_base_url(
                providers.resolved_embedding_base_url(),
                "embedding base URL",
                Some(providers.resolved_embedding_api_key_env()),
            )?;
        }
        if let Some(case_library) = &self.case_library {
            if case_library.embedding_provider == EmbeddingProviderKind::OpenAiCompatible {
                validate_authenticated_provider_base_url(
                    case_library.resolved_embedding_base_url(providers),
                    "case library embedding base URL",
                    Some(case_library.resolved_embedding_api_key_env(providers)),
                )?;
            }
            if case_library.summary_llm_model.is_some() {
                validate_authenticated_provider_base_url(
                    case_library.resolved_summary_base_url(providers),
                    "case library summary LLM base URL",
                    Some(case_library.resolved_summary_api_key_env(providers)),
                )?;
            }
        }
        Ok(())
    }
}

fn validate_case_library_mappings(
    default_library: &str,
    libraries: &[CaseLibraryConfig],
) -> Result<()> {
    if default_library.trim().is_empty() || libraries.is_empty() {
        anyhow::bail!("case library requires a default library and library mappings");
    }

    let mut names = HashSet::with_capacity(libraries.len());
    for library in libraries {
        if library.name.trim().is_empty()
            || library.name.trim() != library.name
            || library.dataset_id.trim().is_empty()
            || library.dataset_id.trim() != library.dataset_id
            || library.tenant_ids.is_empty()
            || library
                .tenant_ids
                .iter()
                .any(|tenant| tenant.trim().is_empty() || tenant.trim() != tenant)
        {
            anyhow::bail!("case library mappings must use canonical non-empty values");
        }
        if !names.insert(library.name.as_str()) {
            anyhow::bail!("case library names must be unique");
        }
    }
    if !names.contains(default_library) {
        anyhow::bail!("default case library must reference a configured library");
    }
    Ok(())
}

fn validate_provider_base_url(value: &str, label: &str) -> Result<url::Url> {
    let (url_scheme, url_remainder) = value
        .split_once("://")
        .with_context(|| format!("{label} must include an HTTP or HTTPS scheme"))?;
    let parsed_base_url =
        url::Url::parse(value).with_context(|| format!("{label} is not a valid URL"))?;
    let authority = url_remainder
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if !matches!(url_scheme.to_ascii_lowercase().as_str(), "http" | "https")
        || parsed_base_url.host_str().is_none()
        || authority.is_empty()
        || !parsed_base_url.username().is_empty()
        || parsed_base_url.password().is_some()
        || parsed_base_url.query().is_some()
        || parsed_base_url.fragment().is_some()
    {
        anyhow::bail!(
            "{label} must be an absolute HTTP or HTTPS URL without credentials, query, or fragment"
        );
    }
    Ok(parsed_base_url)
}

fn validate_authenticated_provider_base_url(
    value: &str,
    label: &str,
    api_key_env: Option<&str>,
) -> Result<url::Url> {
    let parsed_base_url = validate_provider_base_url(value, label)?;
    if api_key_env.is_some()
        && parsed_base_url.scheme() != "https"
        && !is_loopback_or_private(&parsed_base_url)
    {
        anyhow::bail!(
            "{label} must use HTTPS when an API key is configured unless the host is loopback, private, or link-local"
        );
    }
    Ok(parsed_base_url)
}

fn is_loopback_or_private(value: &url::Url) -> bool {
    match value.host() {
        Some(url::Host::Ipv4(address)) => {
            address.is_loopback() || address.is_private() || address.is_link_local()
        }
        Some(url::Host::Ipv6(address)) => {
            if let Some(ipv4) = address.to_ipv4_mapped() {
                return ipv4.is_loopback() || ipv4.is_private() || ipv4.is_link_local();
            }
            address.is_loopback() || address.is_unique_local() || address.is_unicast_link_local()
        }
        Some(url::Host::Domain(domain)) => {
            domain.eq_ignore_ascii_case("localhost")
                || domain.to_ascii_lowercase().ends_with(".localhost")
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        validate_provider_base_url, AuthConfig, CaseLibraryServiceConfig, EmbeddingProviderKind,
        FeaturesConfig, GraphMemoryRetrievalConfig, GraphMemoryServiceConfig, HttpConfig,
        LimitsConfig, PipelineServiceConfig, ProvidersConfig, RerankServiceConfig,
        RetrievalServiceConfig, ServerConfig, TokenConfig, DEFAULT_RERANK_TIMEOUT_MS,
        MAX_ACTIVE_SESSIONS_GLOBAL, MAX_ACTIVE_SESSIONS_PER_PRINCIPAL, MAX_INITIALIZE_RATE_BURST,
        MAX_INITIALIZE_REQUESTS_PER_SECOND, MAX_IN_FLIGHT_PER_PRINCIPAL_TOOL, MAX_MCP_BODY_BYTES,
        MAX_RATE_BURST, MAX_REQUESTS_PER_SECOND, MAX_RERANK_TIMEOUT_MS,
        MAX_SESSION_IDLE_TIMEOUT_SECONDS,
    };
    use memory_core::SearchMode;

    #[test]
    fn feature_http_and_provider_defaults_are_stable() {
        let features = FeaturesConfig::default();
        assert!(features.memory.enabled);
        assert_eq!(features.case_library.enabled, None);
        assert!(!features.graph_memory.enabled);

        let http = HttpConfig::default();
        assert_eq!(http.bind_address.to_string(), "127.0.0.1");
        assert_eq!(http.port, 8080);
        assert!(http.allowed_origins.is_empty());
        assert_eq!(http.allowed_hosts, ["localhost", "127.0.0.1", "::1"]);
        assert!(!http.tls_termination_acknowledged);
        assert!(http.validate_bind().is_ok());

        let providers: ProvidersConfig = serde_json::from_value(serde_json::json!({
            "api_key_env": "RAM_A_PROVIDER_KEY",
            "embedding_model": "embedding-model",
            "embedding_dimensions": 1024,
            "extractor_model": "extractor-model",
            "verifier_model": "verifier-model"
        }))
        .expect("parse provider defaults");
        assert_eq!(providers.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(providers.reasoning_effort, None);
        assert_eq!(providers.enable_thinking, None);
        assert!(providers.send_temperature);
        assert_eq!(providers.temperature, 0.0);
        assert_eq!(
            providers.output_token_parameter,
            memory_pipeline::client::OutputTokenParameter::MaxTokens
        );
        assert_eq!(
            providers.structured_output,
            memory_pipeline::client::StructuredOutputMode::PromptOnly
        );
        assert!(!providers.reasoning_only_retry);
        assert_eq!(providers.json_repair_attempts, 0);
        assert_eq!(
            providers.embedding_provider,
            EmbeddingProviderKind::OpenAiCompatible
        );
        assert_eq!(providers.embedding_api_key_env, None);
        assert_eq!(providers.embedding_base_url, None);
        assert_eq!(
            providers.resolved_embedding_api_key_env(),
            "RAM_A_PROVIDER_KEY"
        );
        assert_eq!(
            providers.resolved_embedding_base_url(),
            "https://openrouter.ai/api/v1"
        );
        assert_eq!(providers.timeout_seconds, 120);
        assert_eq!(providers.max_retries, 3);

        let minimal: ServerConfig = serde_json::from_value(serde_json::json!({
            "auth": {"tokens": []}
        }))
        .expect("parse top-level defaults");
        assert!(minimal.features.memory.enabled);
        assert_eq!(minimal.http.port, 8080);
        assert_eq!(minimal.limits.max_body_bytes, 16 * 1024 * 1024);
        assert!(minimal.pipeline.fail_fast);
        assert_eq!(minimal.pipeline.max_memory_chars, 500);
        assert_eq!(minimal.pipeline.max_candidate_tokens, 320);
        assert_eq!(minimal.pipeline.max_window_tokens, 640);
        assert_eq!(minimal.pipeline.extractor_max_output_tokens, 1600);
        assert_eq!(minimal.pipeline.verifier_max_output_tokens, 1000);
        assert!(minimal.storage.is_none());
        assert!(minimal.providers.is_none());
        assert_eq!(minimal.retrieval.mode, SearchMode::Hybrid);
        assert!(minimal.case_library.is_none());
        assert!(minimal.graph_memory.is_none());
    }

    #[test]
    fn provider_reasoning_effort_is_configurable() {
        let mut source: serde_json::Value =
            serde_json::from_str(include_str!("../../../plugins/mcp/ram-a-mem.json"))
                .expect("packaged config is JSON");
        source["providers"]["reasoning_effort"] = serde_json::json!("none");

        assert!(serde_json::from_value::<ServerConfig>(source).is_ok());
    }

    #[test]
    fn provider_reasoning_effort_rejects_blank_value() {
        let mut config = packaged_config();
        config
            .providers
            .as_mut()
            .expect("providers")
            .reasoning_effort = Some(" ".to_string());

        assert!(config.validate_runtime().is_err());
    }

    #[test]
    fn provider_compatibility_rejects_ambiguous_or_unbounded_values() {
        let mut config = packaged_config();
        let providers = config.providers.as_mut().unwrap();
        providers.reasoning_effort = Some("none".into());
        providers.enable_thinking = Some(false);
        assert!(config.validate_runtime().is_err());

        let mut config = packaged_config();
        config.providers.as_mut().unwrap().json_repair_attempts = 2;
        assert!(config.validate_runtime().is_err());

        let mut config = packaged_config();
        config.providers.as_mut().unwrap().temperature = f64::NAN;
        assert!(config.validate_runtime().is_err());
    }

    #[test]
    fn graph_and_case_library_defaults_are_stable() {
        let graph: GraphMemoryServiceConfig = serde_json::from_value(serde_json::json!({
            "llm_api_key_env": "RAM_A_GRAPH_KEY",
            "llm_model": "graph-model"
        }))
        .expect("parse graph defaults");
        assert_eq!(graph.llm_base_url, "https://openrouter.ai/api/v1");
        assert_eq!(graph.llm_timeout_ms, 60_000);
        assert_eq!(graph.build_concurrency, 1);
        assert_eq!(
            graph.retrieval,
            GraphMemoryRetrievalConfig {
                weight: 0.2,
                rerank_with_graph: false,
                allow_graph_only: false,
                max_graph_only_results: None,
                seed_limit: None,
                max_evidence_records_per_fact: None,
                fail_open: false,
            }
        );
        assert!(graph.validate().is_ok());

        let cases: CaseLibraryServiceConfig = serde_json::from_value(serde_json::json!({
            "default_library": "ops",
            "libraries": [{
                "name": "ops",
                "dataset_id": "ops-dataset",
                "tenant_ids": ["tenant-a"]
            }]
        }))
        .expect("parse case library defaults");
        assert_eq!(
            cases.rag_store.to_string_lossy(),
            "data/memory-cases.sqlite"
        );
        assert_eq!(
            cases.index_store.to_string_lossy(),
            "data/memory-cases-index.sqlite"
        );
        assert_eq!(cases.source_dir, None);
        assert_eq!(cases.api_token_env, None);
        assert_eq!(cases.ingestion_poll_ms, 1_000);
        assert_eq!(
            cases.embedding_provider,
            EmbeddingProviderKind::OpenAiCompatible
        );
        assert_eq!(cases.embedding_api_key_env, None);
        assert_eq!(cases.embedding_base_url, None);
        assert_eq!(cases.embedding_model, "hash");
        assert_eq!(cases.embedding_dimensions, 1_024);
        assert_eq!(cases.chunk_size, 160);
        assert_eq!(cases.summary_llm_model, None);
        assert_eq!(cases.summary_llm_api_key_env, None);
        assert_eq!(cases.summary_llm_base_url, None);
        assert_eq!(cases.summary_llm_timeout_ms, 30_000);
        assert!(cases
            .validate(Some(std::path::Path::new("memory.sqlite")))
            .is_ok());
    }

    #[test]
    fn graph_configuration_rejects_invalid_configurable_values() {
        let valid: GraphMemoryServiceConfig = serde_json::from_value(serde_json::json!({
            "llm_api_key_env": "RAM_A_GRAPH_KEY",
            "llm_model": "graph-model"
        }))
        .expect("parse graph config");

        let mut invalid = Vec::new();
        let mut config = valid.clone();
        config.llm_api_key_env.clear();
        invalid.push(config);
        let mut config = valid.clone();
        config.llm_model.clear();
        invalid.push(config);
        let mut config = valid.clone();
        config.llm_api_key_env = " ".to_string();
        invalid.push(config);
        let mut config = valid.clone();
        config.llm_model = " ".to_string();
        invalid.push(config);
        let mut config = valid.clone();
        config.llm_base_url = "not-a-url".to_string();
        invalid.push(config);
        let mut config = valid.clone();
        config.llm_timeout_ms = 0;
        invalid.push(config);
        let mut config = valid.clone();
        config.build_concurrency = 0;
        invalid.push(config);
        for weight in [-0.1, 1.1, f32::NAN] {
            let mut config = valid.clone();
            config.retrieval.weight = weight;
            invalid.push(config);
        }
        let mut config = valid.clone();
        config.retrieval.max_graph_only_results = Some(0);
        invalid.push(config);
        for seed_limit in [Some(0), Some(memory_core::MAX_GRAPH_SEED_LIMIT + 1)] {
            let mut config = valid.clone();
            config.retrieval.seed_limit = seed_limit;
            invalid.push(config);
        }
        for evidence_limit in [
            Some(0),
            Some(memory_core::MAX_GRAPH_EVIDENCE_RECORDS_PER_FACT + 1),
        ] {
            let mut config = valid.clone();
            config.retrieval.max_evidence_records_per_fact = evidence_limit;
            invalid.push(config);
        }

        assert!(invalid.into_iter().all(|config| config.validate().is_err()));
    }

    #[test]
    fn graph_configuration_accepts_all_documented_boundaries() {
        let base: GraphMemoryServiceConfig = serde_json::from_value(serde_json::json!({
            "llm_api_key_env": "RAM_A_GRAPH_KEY",
            "llm_model": "graph-model"
        }))
        .expect("parse graph config");

        for weight in [0.0, 1.0] {
            let mut config = base.clone();
            config.llm_timeout_ms = 1;
            config.build_concurrency = 1;
            config.retrieval.weight = weight;
            config.retrieval.max_graph_only_results = Some(1);
            config.retrieval.seed_limit = Some(memory_core::MAX_GRAPH_SEED_LIMIT);
            config.retrieval.max_evidence_records_per_fact =
                Some(memory_core::MAX_GRAPH_EVIDENCE_RECORDS_PER_FACT);
            assert!(config.validate().is_ok(), "weight={weight}");
        }

        let mut lower = base;
        lower.retrieval.seed_limit = Some(1);
        lower.retrieval.max_evidence_records_per_fact = Some(1);
        assert!(lower.validate().is_ok());
    }

    #[test]
    fn case_library_configuration_rejects_invalid_configurable_values() {
        let valid: CaseLibraryServiceConfig = serde_json::from_value(serde_json::json!({
            "default_library": "ops",
            "libraries": [{
                "name": "ops",
                "dataset_id": "ops-dataset",
                "tenant_ids": ["tenant-a"]
            }]
        }))
        .expect("parse case library config");

        let validate = |config: &CaseLibraryServiceConfig| {
            config.validate(Some(std::path::Path::new("memory.sqlite")))
        };
        let mut invalid = Vec::new();
        let mut config = valid.clone();
        config.rag_store = ":memory:".into();
        invalid.push(config);
        let mut config = valid.clone();
        config.index_store = config.rag_store.clone();
        invalid.push(config);
        let mut config = valid.clone();
        config.index_store = "memory.sqlite".into();
        invalid.push(config);
        let mut config = valid.clone();
        config.source_dir = Some("".into());
        invalid.push(config);
        let mut config = valid.clone();
        config.api_token_env = Some(" RAM_A_CASE_TOKEN".to_string());
        invalid.push(config);
        let mut config = valid.clone();
        config.ingestion_poll_ms = 0;
        invalid.push(config);
        let mut config = valid.clone();
        config.embedding_model.clear();
        invalid.push(config);
        let mut config = valid.clone();
        config.embedding_dimensions = 0;
        invalid.push(config);
        let mut config = valid.clone();
        config.chunk_size = 0;
        invalid.push(config);
        let mut config = valid.clone();
        config.summary_llm_timeout_ms = 0;
        invalid.push(config);
        let mut config = valid.clone();
        config.summary_llm_model = Some(" ".to_string());
        invalid.push(config);
        let mut config = valid.clone();
        config.embedding_api_key_env = Some(" ".to_string());
        invalid.push(config);
        let mut config = valid.clone();
        config.summary_llm_api_key_env = Some(" ".to_string());
        invalid.push(config);
        let mut config = valid.clone();
        config.default_library = "missing".to_string();
        invalid.push(config);
        let mut config = valid.clone();
        config.libraries.push(config.libraries[0].clone());
        invalid.push(config);

        assert!(invalid.into_iter().all(|config| validate(&config).is_err()));
    }

    #[test]
    fn case_library_positive_only_fields_accept_one() {
        let mut config: CaseLibraryServiceConfig = serde_json::from_value(serde_json::json!({
            "default_library": "ops",
            "libraries": [{
                "name": "ops",
                "dataset_id": "ops-dataset",
                "tenant_ids": ["tenant-a"]
            }]
        }))
        .expect("parse case library config");
        config.ingestion_poll_ms = 1;
        config.embedding_dimensions = 1;
        config.chunk_size = 1;
        config.summary_llm_timeout_ms = 1;
        assert!(config
            .validate(Some(std::path::Path::new("memory.sqlite")))
            .is_ok());
    }

    #[test]
    fn provider_configuration_rejects_incomplete_configurable_values() {
        let valid = packaged_config();
        let mut invalid = Vec::new();
        for field in [
            "api_key_env",
            "base_url",
            "embedding_model",
            "extractor_model",
            "verifier_model",
        ] {
            let mut config = valid.clone();
            let providers = config.providers.as_mut().expect("providers");
            match field {
                "api_key_env" => providers.api_key_env.clear(),
                "base_url" => providers.base_url.clear(),
                "embedding_model" => providers.embedding_model.clear(),
                "extractor_model" => providers.extractor_model.clear(),
                "verifier_model" => providers.verifier_model.clear(),
                _ => unreachable!(),
            }
            invalid.push(config);
        }
        let mut config = valid.clone();
        config
            .providers
            .as_mut()
            .expect("providers")
            .embedding_dimensions = 0;
        invalid.push(config);
        let mut config = valid.clone();
        config
            .providers
            .as_mut()
            .expect("providers")
            .timeout_seconds = 0;
        invalid.push(config);
        let mut config = valid;
        config.providers.as_mut().expect("providers").max_retries = 0;
        invalid.push(config);
        let mut config = packaged_config();
        config
            .providers
            .as_mut()
            .expect("providers")
            .embedding_api_key_env = Some(" ".to_string());
        invalid.push(config);
        let mut config = packaged_config();
        config
            .providers
            .as_mut()
            .expect("providers")
            .embedding_base_url = Some("not-a-url".to_string());
        invalid.push(config);

        assert!(invalid
            .into_iter()
            .all(|config| config.validate_runtime().is_err()));
    }

    #[test]
    fn provider_positive_only_fields_accept_one() {
        let mut config = packaged_config();
        let providers = config.providers.as_mut().expect("providers");
        providers.embedding_dimensions = 1;
        providers.timeout_seconds = 1;
        providers.max_retries = 1;
        assert!(config.validate_runtime().is_ok());
    }

    #[test]
    fn provider_and_case_library_fallbacks_are_explicit_and_overridable() {
        let mut config = packaged_config();
        let providers = config.providers.as_mut().expect("providers");
        providers.api_key_env = "PRIMARY_KEY".to_string();
        providers.base_url = "https://primary.example/v1".to_string();
        providers.embedding_api_key_env = None;
        providers.embedding_base_url = None;
        assert_eq!(providers.resolved_embedding_api_key_env(), "PRIMARY_KEY");
        assert_eq!(
            providers.resolved_embedding_base_url(),
            "https://primary.example/v1"
        );

        let cases = config.case_library.as_ref().expect("case library");
        assert_eq!(
            cases.resolved_embedding_api_key_env(providers),
            "PRIMARY_KEY"
        );
        assert_eq!(
            cases.resolved_embedding_base_url(providers),
            "https://primary.example/v1"
        );
        assert_eq!(cases.resolved_summary_api_key_env(providers), "PRIMARY_KEY");
        assert_eq!(
            cases.resolved_summary_base_url(providers),
            "https://primary.example/v1"
        );

        let providers = config.providers.as_mut().expect("providers");
        providers.embedding_api_key_env = Some("EMBEDDING_KEY".to_string());
        providers.embedding_base_url = Some("https://embedding.example/v1".to_string());
        let cases = config.case_library.as_mut().expect("case library");
        cases.embedding_api_key_env = Some("CASE_EMBEDDING_KEY".to_string());
        cases.embedding_base_url = Some("https://case-embedding.example/v1".to_string());
        cases.summary_llm_model = Some("summary-model".to_string());
        cases.summary_llm_api_key_env = Some("SUMMARY_KEY".to_string());
        cases.summary_llm_base_url = Some("https://summary.example/v1".to_string());

        assert_eq!(providers.resolved_embedding_api_key_env(), "EMBEDDING_KEY");
        assert_eq!(
            providers.resolved_embedding_base_url(),
            "https://embedding.example/v1"
        );
        assert_eq!(
            cases.resolved_embedding_api_key_env(providers),
            "CASE_EMBEDDING_KEY"
        );
        assert_eq!(
            cases.resolved_embedding_base_url(providers),
            "https://case-embedding.example/v1"
        );
        assert_eq!(cases.resolved_summary_api_key_env(providers), "SUMMARY_KEY");
        assert_eq!(
            cases.resolved_summary_base_url(providers),
            "https://summary.example/v1"
        );
        assert!(config.validate_runtime().is_ok());
    }

    #[test]
    fn storage_configuration_rejects_nonpersistent_paths_and_accepts_file_paths() {
        for database_path in ["", ":memory:"] {
            let mut config = packaged_config();
            config.storage.as_mut().expect("storage").database_path = database_path.into();
            assert!(config.validate_runtime().is_err(), "path={database_path}");
        }

        for database_path in [
            "data/ram-a-memory.sqlite",
            "/var/lib/ram-a/ram-a-memory.sqlite",
        ] {
            let mut config = packaged_config();
            config.storage.as_mut().expect("storage").database_path = database_path.into();
            assert!(config.validate_runtime().is_ok(), "path={database_path}");
        }
    }

    #[test]
    fn case_library_paths_and_mappings_reject_every_invalid_shape() {
        let valid = packaged_config();
        let mut invalid = Vec::new();

        for field in ["rag_empty", "index_empty", "index_memory"] {
            let mut config = valid.clone();
            let cases = config.case_library.as_mut().expect("case library");
            match field {
                "rag_empty" => cases.rag_store = "".into(),
                "index_empty" => cases.index_store = "".into(),
                "index_memory" => cases.index_store = ":memory:".into(),
                _ => unreachable!(),
            }
            invalid.push(config);
        }

        for field in [
            "default_empty",
            "libraries_empty",
            "name_empty",
            "name_noncanonical",
            "dataset_empty",
            "dataset_noncanonical",
            "tenants_empty",
            "tenant_empty",
            "tenant_noncanonical",
        ] {
            let mut config = valid.clone();
            let cases = config.case_library.as_mut().expect("case library");
            match field {
                "default_empty" => cases.default_library.clear(),
                "libraries_empty" => cases.libraries.clear(),
                "name_empty" => cases.libraries[0].name.clear(),
                "name_noncanonical" => cases.libraries[0].name = " ops".to_string(),
                "dataset_empty" => cases.libraries[0].dataset_id.clear(),
                "dataset_noncanonical" => {
                    cases.libraries[0].dataset_id = "ops-dataset ".to_string()
                }
                "tenants_empty" => cases.libraries[0].tenant_ids.clear(),
                "tenant_empty" => cases.libraries[0].tenant_ids[0].clear(),
                "tenant_noncanonical" => {
                    cases.libraries[0].tenant_ids[0] = " tenant-local".to_string()
                }
                _ => unreachable!(),
            }
            invalid.push(config);
        }

        for field in ["embedding_url", "summary_url"] {
            let mut config = valid.clone();
            let cases = config.case_library.as_mut().expect("case library");
            match field {
                "embedding_url" => cases.embedding_base_url = Some("not-a-url".to_string()),
                "summary_url" => {
                    cases.summary_llm_model = Some("summary-model".to_string());
                    cases.summary_llm_base_url = Some("not-a-url".to_string());
                }
                _ => unreachable!(),
            }
            invalid.push(config);
        }

        assert!(invalid
            .into_iter()
            .all(|config| config.validate_runtime().is_err()));
    }

    #[test]
    fn http_configuration_covers_host_and_port_boundaries() {
        let default = HttpConfig::default();
        for port in [0_u16, 1, u16::MAX] {
            let config: HttpConfig = serde_json::from_value(serde_json::json!({"port": port}))
                .expect("port must fit u16");
            assert_eq!(config.port, port);
            assert!(config.validate_bind().is_ok());
        }
        assert!(serde_json::from_value::<HttpConfig>(serde_json::json!({"port": 65_536})).is_err());
        assert!(serde_json::from_value::<HttpConfig>(serde_json::json!({"port": -1})).is_err());

        for allowed_hosts in [Vec::new(), vec![String::new()], vec![" ".to_string()]] {
            let config = HttpConfig {
                allowed_hosts,
                ..default.clone()
            };
            assert!(config.validate_bind().is_err());
        }

        let external = HttpConfig {
            bind_address: "0.0.0.0".parse().unwrap(),
            allowed_hosts: vec!["memory.example.test".to_string()],
            tls_termination_acknowledged: true,
            ..default
        };
        assert!(external.validate_bind().is_ok());
    }

    #[test]
    fn authentication_configuration_enforces_fixed_permissions_and_canonical_ids() {
        let valid = AuthConfig {
            tokens: vec![TokenConfig {
                token_env: "RAM_A_TOKEN".to_string(),
                tenant_id: "tenant-a".to_string(),
                user_id: "alice".to_string(),
                agent_id: "xiaoo".to_string(),
                permissions: vec![
                    "memory:read".to_string(),
                    "memory:write".to_string(),
                    "cases:read".to_string(),
                    "cases:write".to_string(),
                ],
            }],
        };
        assert!(valid.validate().is_ok());

        let mut unknown_permission = valid.clone();
        unknown_permission.tokens[0].permissions = vec!["memory:admin".to_string()];
        assert!(unknown_permission.validate().is_err());

        let mut duplicate_permission = valid.clone();
        duplicate_permission.tokens[0].permissions =
            vec!["memory:read".to_string(), "memory:read".to_string()];
        assert!(duplicate_permission.validate().is_err());

        for field in ["token_env", "tenant_id", "user_id", "agent_id"] {
            let mut noncanonical = valid.clone();
            match field {
                "token_env" => noncanonical.tokens[0].token_env = " RAM_A_TOKEN".to_string(),
                "tenant_id" => noncanonical.tokens[0].tenant_id = "tenant-a ".to_string(),
                "user_id" => noncanonical.tokens[0].user_id.clear(),
                "agent_id" => noncanonical.tokens[0].agent_id = " ".to_string(),
                _ => unreachable!(),
            }
            assert!(noncanonical.validate().is_err(), "field={field}");
        }

        let mut duplicate_environment = valid.clone();
        duplicate_environment.tokens.push(valid.tokens[0].clone());
        assert!(duplicate_environment.validate().is_err());
    }

    #[test]
    fn configurable_enums_reject_unsupported_values() {
        assert!(serde_json::from_str::<EmbeddingProviderKind>(r#""hash""#).is_ok());
        assert!(serde_json::from_str::<EmbeddingProviderKind>(r#""openai_compatible""#).is_ok());
        assert_eq!(
            serde_json::from_str::<EmbeddingProviderKind>(r#""open_router""#).unwrap(),
            EmbeddingProviderKind::OpenAiCompatible
        );
        assert!(serde_json::from_str::<EmbeddingProviderKind>(r#""unknown""#).is_err());
        assert!(serde_json::from_str::<SearchMode>(r#""dense""#).is_ok());
        assert!(serde_json::from_str::<SearchMode>(r#""bm25""#).is_ok());
        assert!(serde_json::from_str::<SearchMode>(r#""hybrid""#).is_ok());
        assert!(serde_json::from_str::<SearchMode>(r#""unknown""#).is_err());
        assert!(serde_json::from_str::<memory_core::RerankProvider>(r#""openrouter""#).is_ok());
        assert!(serde_json::from_str::<memory_core::RerankProvider>(r#""unknown""#).is_err());

        let graph_mode = RetrievalServiceConfig {
            mode: SearchMode::Graph,
            ..RetrievalServiceConfig::default()
        };
        assert!(graph_mode.validate().is_err());
    }

    #[test]
    fn pipeline_defaults_and_boundaries_are_stable() {
        let defaults = PipelineServiceConfig::default();
        assert!(defaults.fail_fast);
        assert_eq!(defaults.max_memory_chars, 500);
        assert_eq!(defaults.max_candidate_tokens, 320);
        assert_eq!(defaults.max_window_tokens, 640);
        assert_eq!(defaults.extractor_max_output_tokens, 1600);
        assert_eq!(defaults.verifier_max_output_tokens, 1000);
        assert!(defaults.validate().is_ok());

        for max_memory_chars in [1, crate::MAX_MESSAGE_TEXT_CHARS] {
            let config = PipelineServiceConfig {
                fail_fast: false,
                max_memory_chars,
                ..PipelineServiceConfig::default()
            };
            assert!(config.validate().is_ok());
            let pipeline = config.pipeline_config();
            assert!(!pipeline.fail_fast);
            assert_eq!(pipeline.validation.max_memory_chars, max_memory_chars);
        }
        for max_memory_chars in [0, crate::MAX_MESSAGE_TEXT_CHARS + 1] {
            assert!(PipelineServiceConfig {
                fail_fast: true,
                max_memory_chars,
                ..PipelineServiceConfig::default()
            }
            .validate()
            .is_err());
        }
    }

    #[test]
    fn pipeline_rejects_invalid_model_token_budgets() {
        let mut config = PipelineServiceConfig::default();
        config.max_window_tokens = config.max_candidate_tokens - 1;
        assert!(config.validate().is_err());

        let mut config = PipelineServiceConfig::default();
        config.extractor_max_output_tokens = 0;
        assert!(config.validate().is_err());

        let mut config = PipelineServiceConfig::default();
        config.extractor_context_window_tokens = Some(1600);
        assert!(config.validate().is_err());
    }

    #[test]
    fn http_limit_defaults_are_stable_and_supported() {
        let limits = LimitsConfig::default();
        assert_eq!(limits.max_body_bytes, 16 * 1024 * 1024);
        assert_eq!(limits.requests_per_second, 20);
        assert_eq!(limits.rate_burst, 40);
        assert_eq!(limits.max_in_flight_per_principal_tool, 4);
        assert_eq!(limits.initialize_requests_per_second, 4);
        assert_eq!(limits.initialize_rate_burst, 8);
        assert_eq!(limits.max_active_sessions_per_principal, 8);
        assert_eq!(limits.max_active_sessions_global, 256);
        assert_eq!(limits.session_idle_timeout_seconds, 1_800);
        assert!(limits.validate().is_ok());
    }

    #[test]
    fn http_limits_accept_documented_upper_boundaries() {
        let limits = LimitsConfig {
            max_body_bytes: MAX_MCP_BODY_BYTES,
            requests_per_second: MAX_REQUESTS_PER_SECOND,
            rate_burst: MAX_RATE_BURST,
            max_in_flight_per_principal_tool: MAX_IN_FLIGHT_PER_PRINCIPAL_TOOL,
            initialize_requests_per_second: MAX_INITIALIZE_REQUESTS_PER_SECOND,
            initialize_rate_burst: MAX_INITIALIZE_RATE_BURST,
            max_active_sessions_per_principal: MAX_ACTIVE_SESSIONS_PER_PRINCIPAL,
            max_active_sessions_global: MAX_ACTIVE_SESSIONS_GLOBAL,
            session_idle_timeout_seconds: MAX_SESSION_IDLE_TIMEOUT_SECONDS,
        };
        assert!(limits.validate().is_ok());
    }

    #[test]
    fn http_limits_accept_documented_lower_boundaries() {
        let limits = LimitsConfig {
            max_body_bytes: 1,
            requests_per_second: 1,
            rate_burst: 1,
            max_in_flight_per_principal_tool: 1,
            initialize_requests_per_second: 1,
            initialize_rate_burst: 1,
            max_active_sessions_per_principal: 1,
            max_active_sessions_global: 1,
            session_idle_timeout_seconds: 1,
        };
        assert!(limits.validate().is_ok());
    }

    #[test]
    fn http_limits_reject_zero_out_of_range_and_inconsistent_sessions() {
        let mut invalid = Vec::new();
        macro_rules! invalid_limit {
            ($field:ident, $value:expr) => {{
                let mut limits = LimitsConfig::default();
                limits.$field = $value;
                invalid.push(limits);
            }};
        }
        invalid_limit!(max_body_bytes, 0);
        invalid_limit!(max_body_bytes, MAX_MCP_BODY_BYTES + 1);
        invalid_limit!(requests_per_second, 0);
        invalid_limit!(requests_per_second, MAX_REQUESTS_PER_SECOND + 1);
        invalid_limit!(rate_burst, 0);
        invalid_limit!(rate_burst, MAX_RATE_BURST + 1);
        invalid_limit!(max_in_flight_per_principal_tool, 0);
        invalid_limit!(
            max_in_flight_per_principal_tool,
            MAX_IN_FLIGHT_PER_PRINCIPAL_TOOL + 1
        );
        invalid_limit!(initialize_requests_per_second, 0);
        invalid_limit!(
            initialize_requests_per_second,
            MAX_INITIALIZE_REQUESTS_PER_SECOND + 1
        );
        invalid_limit!(initialize_rate_burst, 0);
        invalid_limit!(initialize_rate_burst, MAX_INITIALIZE_RATE_BURST + 1);
        invalid_limit!(max_active_sessions_per_principal, 0);
        invalid_limit!(
            max_active_sessions_per_principal,
            MAX_ACTIVE_SESSIONS_PER_PRINCIPAL + 1
        );
        invalid_limit!(max_active_sessions_global, 0);
        invalid_limit!(max_active_sessions_global, MAX_ACTIVE_SESSIONS_GLOBAL + 1);
        invalid_limit!(session_idle_timeout_seconds, 0);
        invalid_limit!(
            session_idle_timeout_seconds,
            MAX_SESSION_IDLE_TIMEOUT_SECONDS + 1
        );
        let mut inconsistent = LimitsConfig::default();
        inconsistent.max_active_sessions_per_principal = 9;
        inconsistent.max_active_sessions_global = 8;
        invalid.push(inconsistent);

        assert!(invalid.into_iter().all(|limits| limits.validate().is_err()));
    }

    #[test]
    fn provider_base_url_rejects_credentials_query_and_fragment() {
        for value in [
            "https://user:password@example.com/v1",
            "https://example.com/v1?token=secret",
            "https://example.com/v1#fragment",
        ] {
            assert!(
                validate_provider_base_url(value, "provider").is_err(),
                "{value}"
            );
        }
    }

    #[test]
    fn provider_base_url_allows_trusted_http_endpoints() {
        assert!(validate_provider_base_url("http://127.0.0.1:8080/v1", "provider").is_ok());
        assert!(validate_provider_base_url("https://example.com/v1", "provider").is_ok());
    }

    #[test]
    fn retrieval_defaults_preserve_current_hybrid_behavior() {
        let config = RetrievalServiceConfig::default();
        assert_eq!(config.mode, SearchMode::Hybrid);
        assert_eq!(config.embedding_weight, 0.7);
        assert_eq!(config.bm25_weight, 0.3);
        assert_eq!(config.candidate_k, None);
        assert!(!config.rerank.enabled);
        assert_eq!(
            config.rerank.provider,
            memory_core::RerankProvider::OpenRouter
        );
        assert_eq!(config.rerank.model, "cohere/rerank-v3.5");
        assert_eq!(
            config.rerank.api_key_env.as_deref(),
            Some("OPENROUTER_API_KEY")
        );
        assert_eq!(config.rerank.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(config.rerank.input_k, 40);
        assert_eq!(config.rerank.timeout_ms, Some(DEFAULT_RERANK_TIMEOUT_MS));
        assert!(!config.rerank.fail_open);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn retrieval_parses_openrouter_compatible_local_rerank_without_auth() {
        let config: RetrievalServiceConfig = serde_json::from_value(serde_json::json!({
            "mode": "hybrid",
            "embedding_weight": 0.6,
            "bm25_weight": 0.4,
            "candidate_k": 120,
            "rerank": {
                "enabled": true,
                "provider": "openrouter",
                "model": "local-reranker",
                "api_key_env": null,
                "base_url": "http://127.0.0.1:8081/v1",
                "input_k": 40,
                "timeout_ms": 5000,
                "fail_open": true
            }
        }))
        .expect("parse retrieval config");

        assert!(config.validate().is_ok());
        assert_eq!(config.rerank.api_key_env, None);
        let core = config.core_config(memory_core::GraphRetrievalConfig::default());
        assert!(core.rerank.enabled);
        assert_eq!(core.rerank.model, "local-reranker");
    }

    #[test]
    fn authenticated_rerank_rejects_plain_http_public_host() {
        for base_url in [
            "http://public.example.com/v1",
            "http://[::ffff:8.8.8.8]:8080/v1",
        ] {
            let config = RetrievalServiceConfig {
                rerank: RerankServiceConfig {
                    enabled: true,
                    api_key_env: Some("OPENROUTER_API_KEY".to_string()),
                    base_url: base_url.to_string(),
                    ..RerankServiceConfig::default()
                },
                ..RetrievalServiceConfig::default()
            };

            let error = config.validate().expect_err("public HTTP must be rejected");
            assert!(error.to_string().contains("must use HTTPS"), "{base_url}");
        }
    }

    #[test]
    fn authenticated_rerank_allows_https_and_trusted_http_hosts() {
        for base_url in [
            "https://public.example.com/v1",
            "http://localhost:8080/v1",
            "http://127.0.0.1:8080/v1",
            "http://10.0.0.1:8080/v1",
            "http://169.254.1.1:8080/v1",
            "http://[::1]:8080/v1",
            "http://[::ffff:127.0.0.1]:8080/v1",
            "http://[fd00::1]:8080/v1",
            "http://[fe80::1]:8080/v1",
        ] {
            let config = RetrievalServiceConfig {
                rerank: RerankServiceConfig {
                    enabled: true,
                    api_key_env: Some("OPENROUTER_API_KEY".to_string()),
                    base_url: base_url.to_string(),
                    ..RerankServiceConfig::default()
                },
                ..RetrievalServiceConfig::default()
            };

            assert!(config.validate().is_ok(), "{base_url}");
        }
    }

    #[test]
    fn authenticated_runtime_providers_reject_plain_http_public_hosts() {
        let mut config = packaged_config();
        config.providers.as_mut().expect("providers").base_url =
            "http://public.example.com/v1".to_string();
        assert!(config
            .validate_runtime()
            .expect_err("primary provider public HTTP must be rejected")
            .to_string()
            .contains("provider base URL must use HTTPS"));

        let mut config = packaged_config();
        let providers = config.providers.as_mut().expect("providers");
        providers.embedding_provider = EmbeddingProviderKind::OpenAiCompatible;
        providers.embedding_base_url = Some("http://public.example.com/v1".to_string());
        assert!(config
            .validate_runtime()
            .expect_err("embedding provider public HTTP must be rejected")
            .to_string()
            .contains("embedding base URL must use HTTPS"));

        let mut config = packaged_config();
        let graph = config.graph_memory.as_mut().expect("graph memory");
        graph.llm_base_url = "http://public.example.com/v1".to_string();
        assert!(config
            .validate_runtime()
            .expect_err("graph provider public HTTP must be rejected")
            .to_string()
            .contains("graph memory LLM base URL must use HTTPS"));

        let mut config = packaged_config();
        let case_library = config.case_library.as_mut().expect("case library");
        case_library.embedding_provider = EmbeddingProviderKind::OpenAiCompatible;
        case_library.embedding_base_url = Some("http://public.example.com/v1".to_string());
        assert!(config
            .validate_runtime()
            .expect_err("case embedding public HTTP must be rejected")
            .to_string()
            .contains("case library embedding base URL must use HTTPS"));

        let mut config = packaged_config();
        let case_library = config.case_library.as_mut().expect("case library");
        case_library.summary_llm_model = Some("summary-model".to_string());
        case_library.summary_llm_base_url = Some("http://public.example.com/v1".to_string());
        assert!(config
            .validate_runtime()
            .expect_err("case summary public HTTP must be rejected")
            .to_string()
            .contains("case library summary LLM base URL must use HTTPS"));
    }

    #[test]
    fn retrieval_rejects_invalid_weights_and_non_hybrid_rerank() {
        for (embedding_weight, bm25_weight) in [
            (0.8, 0.3),
            (-0.1, 1.1),
            (1.1, -0.1),
            (f32::NAN, 0.0),
            (f32::INFINITY, 0.0),
        ] {
            let invalid_weights = RetrievalServiceConfig {
                embedding_weight,
                bm25_weight,
                ..RetrievalServiceConfig::default()
            };
            assert!(
                invalid_weights.validate().is_err(),
                "embedding_weight={embedding_weight}, bm25_weight={bm25_weight}"
            );
        }

        let dense_rerank = RetrievalServiceConfig {
            mode: SearchMode::Dense,
            rerank: RerankServiceConfig {
                enabled: true,
                ..RerankServiceConfig::default()
            },
            ..RetrievalServiceConfig::default()
        };
        assert!(dense_rerank.validate().is_err());
    }

    #[test]
    fn retrieval_accepts_hybrid_weight_boundaries() {
        for (embedding_weight, bm25_weight) in [(0.0, 1.0), (1.0, 0.0), (0.7, 0.3)] {
            let config = RetrievalServiceConfig {
                embedding_weight,
                bm25_weight,
                ..RetrievalServiceConfig::default()
            };
            assert!(config.validate().is_ok());
        }
    }

    #[test]
    fn disabled_rerank_ignores_inactive_provider_fields() {
        let config = RetrievalServiceConfig {
            rerank: RerankServiceConfig {
                enabled: false,
                model: String::new(),
                api_key_env: Some(String::new()),
                base_url: String::new(),
                input_k: 0,
                timeout_ms: None,
                fail_open: true,
                ..RerankServiceConfig::default()
            },
            ..RetrievalServiceConfig::default()
        };

        assert!(config.validate().is_ok());
        let core = config.core_config(memory_core::GraphRetrievalConfig::default());
        assert!(!core.rerank.enabled);
        assert!(core.rerank.fail_open);
    }

    #[test]
    fn enabled_rerank_rejects_every_invalid_provider_field() {
        let mut invalid = Vec::new();
        for field in ["model", "base_url", "api_key_env"] {
            let mut config = RetrievalServiceConfig::default();
            config.rerank.enabled = true;
            match field {
                "model" => config.rerank.model = " ".to_string(),
                "base_url" => config.rerank.base_url = " ".to_string(),
                "api_key_env" => config.rerank.api_key_env = Some(" ".to_string()),
                _ => unreachable!(),
            }
            invalid.push(config);
        }
        let mut invalid_url = RetrievalServiceConfig::default();
        invalid_url.rerank.enabled = true;
        invalid_url.rerank.base_url = "not-a-url".to_string();
        invalid.push(invalid_url);

        assert!(invalid.into_iter().all(|config| config.validate().is_err()));
    }

    #[test]
    fn retrieval_candidate_and_rerank_limits_accept_boundaries() {
        for candidate_k in [1, 500] {
            let config = RetrievalServiceConfig {
                candidate_k: Some(candidate_k),
                ..RetrievalServiceConfig::default()
            };
            assert!(config.validate().is_ok(), "candidate_k={candidate_k}");
        }

        for input_k in [1, 500] {
            let config = RetrievalServiceConfig {
                rerank: RerankServiceConfig {
                    enabled: true,
                    input_k,
                    timeout_ms: Some(1),
                    ..RerankServiceConfig::default()
                },
                ..RetrievalServiceConfig::default()
            };
            assert!(config.validate().is_ok(), "input_k={input_k}");
        }

        for timeout_ms in [1, MAX_RERANK_TIMEOUT_MS] {
            let config = RetrievalServiceConfig {
                rerank: RerankServiceConfig {
                    enabled: true,
                    timeout_ms: Some(timeout_ms),
                    ..RerankServiceConfig::default()
                },
                ..RetrievalServiceConfig::default()
            };
            assert!(config.validate().is_ok(), "timeout_ms={timeout_ms}");
        }
    }

    #[test]
    fn retrieval_rejects_candidate_and_rerank_values_outside_limits() {
        for candidate_k in [0, 501] {
            let config = RetrievalServiceConfig {
                candidate_k: Some(candidate_k),
                ..RetrievalServiceConfig::default()
            };
            assert!(config.validate().is_err(), "candidate_k={candidate_k}");
        }

        for input_k in [0, 501] {
            let config = RetrievalServiceConfig {
                rerank: RerankServiceConfig {
                    enabled: true,
                    input_k,
                    ..RerankServiceConfig::default()
                },
                ..RetrievalServiceConfig::default()
            };
            assert!(config.validate().is_err(), "input_k={input_k}");
        }

        let zero_timeout = RetrievalServiceConfig {
            rerank: RerankServiceConfig {
                enabled: true,
                timeout_ms: Some(0),
                ..RerankServiceConfig::default()
            },
            ..RetrievalServiceConfig::default()
        };
        assert!(zero_timeout.validate().is_err());

        for timeout_ms in [None, Some(MAX_RERANK_TIMEOUT_MS + 1)] {
            let invalid_timeout = RetrievalServiceConfig {
                rerank: RerankServiceConfig {
                    enabled: true,
                    timeout_ms,
                    ..RerankServiceConfig::default()
                },
                ..RetrievalServiceConfig::default()
            };
            assert!(invalid_timeout.validate().is_err());
        }
    }

    #[test]
    fn packaged_rpm_example_matches_server_schema() {
        let source: serde_json::Value =
            serde_json::from_str(include_str!("../../../plugins/mcp/ram-a-mem.json"))
                .expect("packaged config is JSON");
        let config: ServerConfig =
            serde_json::from_value(source.clone()).expect("packaged config parses");

        assert!(config.validate_runtime().is_ok());
        assert_eq!(
            serde_json::to_value(&config).expect("config serializes"),
            source,
            "the full example must explicitly contain every serializable config field"
        );
        assert_eq!(config.limits.max_body_bytes, 16 * 1024 * 1024);
        assert!(config.pipeline.fail_fast);
        assert_eq!(config.pipeline.max_memory_chars, 500);
        assert_eq!(config.retrieval.mode, SearchMode::Hybrid);
        assert_eq!(config.retrieval.embedding_weight, 0.7);
        assert_eq!(config.retrieval.bm25_weight, 0.3);
    }

    fn packaged_config() -> ServerConfig {
        serde_json::from_str(include_str!("../../../plugins/mcp/ram-a-mem.json"))
            .expect("packaged config parses")
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    pub tokens: Vec<TokenConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TokenConfig {
    pub token_env: String,
    pub tenant_id: String,
    pub user_id: String,
    pub agent_id: String,
    pub permissions: Vec<String>,
}

impl AuthConfig {
    fn validate(&self) -> Result<()> {
        if self.tokens.is_empty() {
            anyhow::bail!("production runtime requires at least one authenticated principal");
        }

        let mut token_environments = HashSet::with_capacity(self.tokens.len());
        for token in &self.tokens {
            for (label, value) in [
                ("token_env", token.token_env.as_str()),
                ("tenant_id", token.tenant_id.as_str()),
                ("user_id", token.user_id.as_str()),
                ("agent_id", token.agent_id.as_str()),
            ] {
                if value.trim().is_empty() || value.trim() != value {
                    anyhow::bail!("authentication {label} must be canonical and non-empty");
                }
            }
            if !token_environments.insert(token.token_env.as_str()) {
                anyhow::bail!("authentication token environment names must be unique");
            }

            let mut permissions = HashSet::with_capacity(token.permissions.len());
            for permission in &token.permissions {
                if !SUPPORTED_PERMISSIONS.contains(&permission.as_str()) {
                    anyhow::bail!("authentication permission `{permission}` is not supported");
                }
                if !permissions.insert(permission.as_str()) {
                    anyhow::bail!("authentication permissions must be unique per token");
                }
            }
        }
        Ok(())
    }
}

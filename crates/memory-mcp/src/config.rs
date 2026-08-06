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
    pub storage: Option<StorageConfig>,
    #[serde(default)]
    pub providers: Option<ProvidersConfig>,
    #[serde(default)]
    pub case_library: Option<CaseLibraryServiceConfig>,
    #[serde(default)]
    pub graph_memory: Option<GraphMemoryServiceConfig>,
}

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
        validate_provider_base_url(&self.llm_base_url, "graph memory LLM base URL")?;
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
    1_048_576
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
        if self.features.case_library.enabled == Some(true) && self.case_library.is_none() {
            anyhow::bail!("case_library feature requires case_library configuration");
        }
        if self.features.graph_memory.enabled && self.graph_memory.is_none() {
            anyhow::bail!("graph_memory feature requires graph_memory configuration");
        }
        if self.features.graph_memory.enabled && !self.features.memory.enabled {
            anyhow::bail!("graph_memory feature requires the memory feature");
        }
        if self.auth.tokens.is_empty() {
            anyhow::bail!("production runtime requires at least one authenticated principal");
        }
        if self.limits.max_body_bytes == 0
            || self.limits.requests_per_second == 0
            || self.limits.rate_burst == 0
            || self.limits.max_in_flight_per_principal_tool == 0
            || self.limits.initialize_requests_per_second == 0
            || self.limits.initialize_rate_burst == 0
            || self.limits.max_active_sessions_per_principal == 0
            || self.limits.max_active_sessions_global == 0
            || self.limits.session_idle_timeout_seconds == 0
        {
            anyhow::bail!("HTTP limits must all be non-zero");
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
        validate_provider_base_url(&providers.base_url, "provider base URL")?;
        if let Some(embedding_api_key_env) = providers.embedding_api_key_env.as_deref() {
            if embedding_api_key_env.trim().is_empty() {
                anyhow::bail!("embedding API key environment name must not be empty");
            }
        }
        if let Some(embedding_base_url) = providers.embedding_base_url.as_deref() {
            validate_provider_base_url(embedding_base_url, "embedding base URL")?;
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

fn validate_provider_base_url(value: &str, label: &str) -> Result<()> {
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_provider_base_url;

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

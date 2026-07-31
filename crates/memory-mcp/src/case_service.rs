use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{CaseSearchRequest, CaseServiceConfig, Principal};

const MAX_REFERENCE_CHARS: usize = 4_000;

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct CaseSearchResponse {
    pub library: String,
    pub references: Vec<CaseReference>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct CaseReference {
    pub chunk_id: String,
    pub document_id: String,
    pub source_name: Option<String>,
    pub content: String,
    pub score: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaseServiceError {
    InvalidRequest,
    Forbidden,
    Unavailable,
    InvalidResponse,
    NotConfigured,
}

impl CaseServiceError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "CASE_INVALID_REQUEST",
            Self::Forbidden => "CASE_FORBIDDEN",
            Self::Unavailable => "CASE_UNAVAILABLE",
            Self::InvalidResponse => "CASE_INVALID_RESPONSE",
            Self::NotConfigured => "CASE_NOT_CONFIGURED",
        }
    }

    pub fn retriable(self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

impl fmt::Display for CaseServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRequest => "case search request is invalid",
            Self::Forbidden => "case library access is forbidden",
            Self::Unavailable => "case service is unavailable",
            Self::InvalidResponse => "case service returned an invalid response",
            Self::NotConfigured => "case service is not configured",
        })
    }
}

impl std::error::Error for CaseServiceError {}

#[async_trait]
pub trait CaseSearchProvider: Send + Sync {
    async fn search(
        &self,
        principal: &Principal,
        request: CaseSearchRequest,
    ) -> Result<CaseSearchResponse, CaseServiceError>;
}

pub type DynCaseSearchProvider = Arc<dyn CaseSearchProvider>;

#[derive(Clone)]
struct LibraryMapping {
    dataset_id: String,
    tenant_ids: HashSet<String>,
}

#[derive(Clone)]
pub struct CaseServiceClient {
    http: reqwest::Client,
    base_url: url::Url,
    bearer_token: Arc<str>,
    max_response_bytes: usize,
    default_library: String,
    libraries: HashMap<String, LibraryMapping>,
}

impl CaseServiceClient {
    pub fn from_config(config: &CaseServiceConfig) -> Result<Self> {
        config.validate()?;
        let token = std::env::var_os(&config.bearer_token_env).with_context(|| {
            format!(
                "case service credential environment variable `{}` is unavailable",
                config.bearer_token_env
            )
        })?;
        let token = token.into_string().map_err(|_| {
            anyhow::anyhow!(
                "case service credential environment variable `{}` is not valid Unicode",
                config.bearer_token_env
            )
        })?;
        if token.trim().is_empty() || token.trim() != token {
            bail!(
                "case service credential environment variable `{}` must be canonical and non-empty",
                config.bearer_token_env
            );
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()
            .context("failed to construct case service HTTP client")?;
        let libraries = config
            .libraries
            .iter()
            .map(|library| {
                (
                    library.name.clone(),
                    LibraryMapping {
                        dataset_id: library.dataset_id.clone(),
                        tenant_ids: library.tenant_ids.iter().cloned().collect(),
                    },
                )
            })
            .collect();
        Ok(Self {
            http,
            base_url: url::Url::parse(&config.base_url)
                .context("case service base URL is not valid")?,
            bearer_token: Arc::from(token),
            max_response_bytes: config.max_response_bytes,
            default_library: config.default_library.clone(),
            libraries,
        })
    }

    fn search_url(&self, dataset_id: &str) -> Result<url::Url, CaseServiceError> {
        let mut url = self.base_url.clone();
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| CaseServiceError::InvalidResponse)?;
        segments.pop_if_empty();
        segments.extend(["api", "v1", "datasets", dataset_id, "search"]);
        drop(segments);
        Ok(url)
    }
}

#[derive(Serialize)]
struct UpstreamSearchRequest<'a> {
    query: &'a str,
    top_k: usize,
}

#[derive(Deserialize)]
struct UpstreamSearchResponse {
    chunks: Vec<UpstreamChunk>,
}

#[derive(Deserialize)]
struct UpstreamChunk {
    chunk_id: String,
    dataset_id: String,
    document_id: String,
    source_name: Option<String>,
    content: String,
    score: f32,
}

#[async_trait]
impl CaseSearchProvider for CaseServiceClient {
    async fn search(
        &self,
        principal: &Principal,
        request: CaseSearchRequest,
    ) -> Result<CaseSearchResponse, CaseServiceError> {
        request
            .validate()
            .map_err(|_| CaseServiceError::InvalidRequest)?;
        let library_name = request.library.as_deref().unwrap_or(&self.default_library);
        let library = self
            .libraries
            .get(library_name)
            .filter(|library| library.tenant_ids.contains(&principal.tenant_id))
            .ok_or(CaseServiceError::Forbidden)?;
        let url = self.search_url(&library.dataset_id)?;
        let mut response = self
            .http
            .post(url)
            .bearer_auth(self.bearer_token.as_ref())
            .json(&UpstreamSearchRequest {
                query: &request.query,
                top_k: request.top_k,
            })
            .send()
            .await
            .map_err(|_| CaseServiceError::Unavailable)?;
        if !response.status().is_success() {
            return Err(CaseServiceError::Unavailable);
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.max_response_bytes as u64)
        {
            return Err(CaseServiceError::InvalidResponse);
        }

        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| CaseServiceError::Unavailable)?
        {
            if bytes.len().saturating_add(chunk.len()) > self.max_response_bytes {
                return Err(CaseServiceError::InvalidResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        let upstream: UpstreamSearchResponse =
            serde_json::from_slice(&bytes).map_err(|_| CaseServiceError::InvalidResponse)?;

        let mut truncated = upstream.chunks.len() > request.top_k;
        let mut references = Vec::with_capacity(upstream.chunks.len().min(request.top_k));
        for chunk in upstream.chunks.into_iter().take(request.top_k) {
            if chunk.dataset_id != library.dataset_id
                || chunk.chunk_id.trim().is_empty()
                || chunk.document_id.trim().is_empty()
                || chunk.content.trim().is_empty()
                || !chunk.score.is_finite()
            {
                return Err(CaseServiceError::InvalidResponse);
            }
            let (content, content_truncated) = truncate_chars(&chunk.content, MAX_REFERENCE_CHARS);
            truncated |= content_truncated;
            references.push(CaseReference {
                chunk_id: chunk.chunk_id,
                document_id: chunk.document_id,
                source_name: chunk.source_name,
                content,
                score: chunk.score,
            });
        }
        Ok(CaseSearchResponse {
            library: library_name.to_owned(),
            references,
            truncated,
        })
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> (String, bool) {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        (truncated, true)
    } else {
        (truncated, false)
    }
}

#[derive(Default)]
pub struct DisabledCaseSearchProvider;

#[async_trait]
impl CaseSearchProvider for DisabledCaseSearchProvider {
    async fn search(
        &self,
        _principal: &Principal,
        _request: CaseSearchRequest,
    ) -> Result<CaseSearchResponse, CaseServiceError> {
        Err(CaseServiceError::NotConfigured)
    }
}

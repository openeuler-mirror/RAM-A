//! Contracts shared by the RAM-A MCP transport and service layers.

pub mod auth;
pub mod case_service;
pub mod config;
pub mod http;
pub mod idempotency;
pub mod mcp_server;
pub mod service;
pub mod types;

pub use auth::{Principal, TokenAuthenticator};
pub use case_service::{
    CaseReference, CaseSearchProvider, CaseSearchResponse, CaseServiceClient, CaseServiceError,
    DisabledCaseSearchProvider, DynCaseSearchProvider,
};
pub use config::{
    AuthConfig, CaseLibraryConfig, CaseLibraryFeatureConfig, CaseServiceConfig,
    EmbeddingProviderKind, FeatureFlags, FeaturesConfig, HttpConfig, LimitsConfig,
    MemoryFeatureConfig, ProvidersConfig, ServerConfig, StorageConfig, TokenConfig,
};
pub use http::{create_http_router, HttpRuntime, RequestId, AGENT_ID_HEADER, REQUEST_ID_HEADER};
pub use idempotency::IdempotencyRepository;
pub use mcp_server::{DynMemoryService, MemoryMcpServer};
pub use service::{IngestResponse, MemoryService, SearchResponse, SearchResult, ServiceError};
pub use types::{
    CaseSearchRequest, IngestMessage, IngestRequest, SearchRequest, MAX_CASE_TOP_K,
    MAX_INGEST_MESSAGES, MAX_MESSAGE_TEXT_CHARS, MAX_QUERY_CHARS, MAX_TOP_K,
};

//! Contracts shared by the RAM-A MCP transport and service layers.

pub mod auth;
pub mod case_service;
pub mod config;
pub mod http;
pub mod idempotency;
pub mod mcp_server;
pub mod observability;
pub mod service;
pub mod types;

pub use auth::{Principal, TokenAuthenticator};
pub use case_service::{
    CaseDeleteProposalResponse, CaseDocumentDeleteResponse, CaseDocumentMutationResponse,
    CaseMutationProposalResponse, CaseReference, CaseSearchProvider, CaseSearchResponse,
    CaseServiceClient, CaseServiceError, DisabledCaseSearchProvider, DynCaseSearchProvider,
    EmbeddedCaseSearchProvider,
};
pub use config::{
    AuthConfig, CaseLibraryConfig, CaseLibraryFeatureConfig, CaseLibraryServiceConfig,
    CaseServiceConfig, EmbeddingProviderKind, FeatureFlags, FeaturesConfig,
    GraphMemoryFeatureConfig, GraphMemoryRetrievalConfig, GraphMemoryServiceConfig, HttpConfig,
    LimitsConfig, MemoryFeatureConfig, PipelineServiceConfig, ProvidersConfig, RerankServiceConfig,
    RetrievalServiceConfig, ServerConfig, StorageConfig, TokenConfig, DEFAULT_RERANK_TIMEOUT_MS,
    MAX_ACTIVE_SESSIONS_GLOBAL, MAX_ACTIVE_SESSIONS_PER_PRINCIPAL, MAX_INITIALIZE_RATE_BURST,
    MAX_INITIALIZE_REQUESTS_PER_SECOND, MAX_IN_FLIGHT_PER_PRINCIPAL_TOOL, MAX_MCP_BODY_BYTES,
    MAX_RATE_BURST, MAX_REQUESTS_PER_SECOND, MAX_RERANK_TIMEOUT_MS,
    MAX_SESSION_IDLE_TIMEOUT_SECONDS,
};
pub use http::{create_http_router, HttpRuntime, RequestId, AGENT_ID_HEADER, REQUEST_ID_HEADER};
pub use idempotency::IdempotencyRepository;
pub use mcp_server::{DynMemoryService, MemoryMcpServer};
pub use service::{IngestResponse, MemoryService, SearchResponse, SearchResult, ServiceError};
pub use types::{
    CaseDocumentDeleteRequest, CaseDocumentUpdateRequest, CaseDocumentUploadRequest,
    CaseMutationConfirmationRequest, CaseSearchRequest, IngestMessage, IngestRequest,
    SearchRequest, MAX_CASE_DELETION_REASON_CHARS, MAX_CASE_DIAGNOSIS_CHARS,
    MAX_CASE_DOCUMENT_CHARS, MAX_CASE_DOCUMENT_ID_CHARS, MAX_CASE_DOCUMENT_NAME_CHARS,
    MAX_CASE_FILE_NAME_CHARS, MAX_CASE_LIBRARY_CHARS, MAX_CASE_TOP_K, MAX_CONVERSATION_ID_CHARS,
    MAX_INGEST_MESSAGES, MAX_MESSAGE_ID_CHARS, MAX_MESSAGE_TEXT_CHARS, MAX_QUERY_CHARS,
    MAX_SPEAKER_CHARS, MAX_TOP_K,
};

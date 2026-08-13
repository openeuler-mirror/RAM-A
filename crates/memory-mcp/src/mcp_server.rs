use std::time::Instant;

use memory_pipeline::extraction::MemoryExtractor;
use memory_pipeline::grounding::GroundingVerifier;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::{Extension, ToolCallContext};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ListToolsResult, PaginatedRequestParams,
    ProtocolVersion, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{tool, tool_router, ErrorData as McpError, ServerHandler};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::{
    CaseSearchRequest, CaseSearchResponse, CaseServiceError, DisabledCaseSearchProvider,
    DynCaseSearchProvider, FeatureFlags, IngestRequest, IngestResponse, MemoryService, Principal,
    RequestId, SearchRequest, SearchResponse, ServiceError,
};

pub type DynMemoryService = MemoryService<dyn MemoryExtractor, dyn GroundingVerifier>;

#[derive(Clone)]
pub struct MemoryMcpServer {
    service: DynMemoryService,
    case_search: DynCaseSearchProvider,
    features: FeatureFlags,
    cancellation_token: CancellationToken,
    tool_router: ToolRouter<Self>,
}

impl MemoryMcpServer {
    pub fn new(service: DynMemoryService) -> Self {
        Self::with_cancellation_token(service, CancellationToken::new())
    }

    pub fn with_cancellation_token(
        service: DynMemoryService,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self::with_case_search_provider(
            service,
            cancellation_token,
            std::sync::Arc::new(DisabledCaseSearchProvider),
            FeatureFlags::all(),
        )
    }

    pub fn with_case_search_provider(
        service: DynMemoryService,
        cancellation_token: CancellationToken,
        case_search: DynCaseSearchProvider,
        features: FeatureFlags,
    ) -> Self {
        let mut tool_router = Self::tool_router();
        if !features.memory {
            tool_router.disable_route("memory_ingest");
            tool_router.disable_route("memory_search");
        }
        if !features.case_library {
            tool_router.disable_route("memory_case_search");
        }
        Self {
            service,
            case_search,
            features,
            cancellation_token,
            tool_router,
        }
    }

    fn disabled_tool_code(&self, name: &str) -> Option<&'static str> {
        match name {
            "memory_ingest" | "memory_search" if !self.features.memory => Some("MEMORY_DISABLED"),
            "memory_case_search" if !self.features.case_library => Some("CASE_LIBRARY_DISABLED"),
            _ => None,
        }
    }
}

#[tool_router]
impl MemoryMcpServer {
    #[tool(
        name = "memory_ingest",
        description = "Extract, ground, and persist authenticated conversation memories",
        output_schema = rmcp::handler::server::tool::schema_for_type::<IngestResponse>()
    )]
    async fn memory_ingest(
        &self,
        Parameters(request): Parameters<IngestRequest>,
        Extension(parts): Extension<axum::http::request::Parts>,
    ) -> CallToolResult {
        let Some(principal) = parts.extensions.get::<Principal>() else {
            return tool_error("UNAUTHORIZED", false);
        };
        if !principal
            .permissions
            .iter()
            .any(|permission| permission == "memory:write")
        {
            return tool_error("FORBIDDEN", false);
        }
        let request_id = request_id(&parts);
        let span = tool_span("memory_ingest", &request_id, principal);
        async {
            let started = Instant::now();
            let message_count = request.messages.len();
            let candidate_count = request
                .messages
                .iter()
                .filter(|message| message.candidate)
                .count();
            tracing::info!(
                event = "ram_a.memory.ingest.started",
                message_count,
                candidate_count
            );
            let result = tokio::select! {
                biased;
                _ = self.cancellation_token.cancelled() => {
                    tracing::warn!(event = "ram_a.memory.ingest.failed", stage = "cancelled", error_code = "CANCELLED", retriable = true, latency_ms = started.elapsed().as_millis() as u64);
                    return tool_error("CANCELLED", true);
                }
                result = self.service.ingest(principal, request) => result,
            };
            match result {
                Ok(response) => {
                    tracing::info!(event = "ram_a.memory.ingest.records", record_ids = ?response.memory_ids);
                    tracing::info!(
                        event = "ram_a.memory.ingest.completed",
                        pipeline_run_id = %response.pipeline_run_id,
                        record_count = response.memory_ids.len(),
                        accepted_count = response.accepted_count,
                        rejected_count = response.rejected_count,
                        quarantined_count = response.quarantined_count,
                        idempotency_hit = response.idempotency_hit,
                        latency_ms = started.elapsed().as_millis() as u64
                    );
                    CallToolResult::structured(
                        serde_json::to_value(response).expect("ingest response is serializable"),
                    )
                }
                Err(error) => {
                    tracing::error!(event = "ram_a.memory.ingest.failed", stage = "service", error_code = error.code(), retriable = error.retriable(), latency_ms = started.elapsed().as_millis() as u64);
                    service_error(error)
                }
            }
        }
        .instrument(span)
        .await
    }

    #[tool(
        name = "memory_search",
        description = "Search the authenticated user's shared long-term memory",
        output_schema = rmcp::handler::server::tool::schema_for_type::<SearchResponse>()
    )]
    async fn memory_search(
        &self,
        Parameters(request): Parameters<SearchRequest>,
        Extension(parts): Extension<axum::http::request::Parts>,
    ) -> CallToolResult {
        let Some(principal) = parts.extensions.get::<Principal>() else {
            return tool_error("UNAUTHORIZED", false);
        };
        if !principal
            .permissions
            .iter()
            .any(|permission| permission == "memory:read")
        {
            return tool_error("FORBIDDEN", false);
        }
        let request_id = request_id(&parts);
        let span = tool_span("memory_search", &request_id, principal);
        async {
            let started = Instant::now();
            let top_k = request.top_k;
            tracing::info!(event = "ram_a.memory.search.started", top_k);
            let result = tokio::select! {
                biased;
                _ = self.cancellation_token.cancelled() => {
                    tracing::warn!(event = "ram_a.memory.search.failed", stage = "cancelled", error_code = "CANCELLED", retriable = true, latency_ms = started.elapsed().as_millis() as u64);
                    return tool_error("CANCELLED", true);
                }
                result = self.service.search(principal, request) => result,
            };
            match result {
                Ok(response) => {
                    tracing::info!(event = "ram_a.memory.search.completed", result_count = response.memories.len(), latency_ms = started.elapsed().as_millis() as u64);
                    CallToolResult::structured(
                        serde_json::to_value(response).expect("search response is serializable"),
                    )
                }
                Err(error) => {
                    tracing::error!(event = "ram_a.memory.search.failed", stage = "service", error_code = error.code(), retriable = error.retriable(), latency_ms = started.elapsed().as_millis() as u64);
                    service_error(error)
                }
            }
        }
        .instrument(span)
        .await
    }

    #[tool(
        name = "memory_case_search",
        description = "First use this tool for operational troubleshooting, incident diagnosis, case lookup, root-cause analysis, remediation steps, or similar historical case questions. Also use this tool when the user asks whether there were similar past cases, previous incidents, known fixes, or examples for a troubleshooting symptom, even if they do not explicitly say \"case library\" or name the tool. It searches an authorized case library and returns evidence plus source references; base troubleshooting answers on those returned references instead of general knowledge alone.",
        output_schema = rmcp::handler::server::tool::schema_for_type::<CaseSearchResponse>()
    )]
    async fn memory_case_search(
        &self,
        Parameters(request): Parameters<CaseSearchRequest>,
        Extension(parts): Extension<axum::http::request::Parts>,
    ) -> CallToolResult {
        let Some(principal) = parts.extensions.get::<Principal>() else {
            return tool_error("UNAUTHORIZED", false);
        };
        if !principal
            .permissions
            .iter()
            .any(|permission| permission == "cases:read")
        {
            return tool_error("FORBIDDEN", false);
        }
        let request_id = request_id(&parts);
        let span = tool_span("memory_case_search", &request_id, principal);
        async {
            let started = Instant::now();
            tracing::info!(event = "ram_a.case.search.started", top_k = request.top_k);
            let result = tokio::select! {
                biased;
                _ = self.cancellation_token.cancelled() => {
                    tracing::warn!(event = "ram_a.case.search.failed", stage = "cancelled", error_code = "CANCELLED", retriable = true, latency_ms = started.elapsed().as_millis() as u64);
                    return tool_error("CANCELLED", true);
                }
                result = self.case_search.search(principal, request) => result,
            };
            match result {
                Ok(response) => {
                    tracing::info!(event = "ram_a.case.search.completed", result_count = response.references.len(), latency_ms = started.elapsed().as_millis() as u64);
                    CallToolResult::structured(
                        serde_json::to_value(response).expect("case search response is serializable"),
                    )
                }
                Err(error) => {
                    tracing::error!(event = "ram_a.case.search.failed", stage = "service", error_code = error.code(), retriable = error.retriable(), latency_ms = started.elapsed().as_millis() as u64);
                    case_service_error(error)
                }
            }
        }
        .instrument(span)
        .await
    }
}

fn request_id(parts: &axum::http::request::Parts) -> String {
    parts
        .extensions
        .get::<RequestId>()
        .map(|request_id| request_id.0.clone())
        .unwrap_or_else(|| "unavailable".to_string())
}

fn tool_span(tool: &'static str, request_id: &str, principal: &Principal) -> tracing::Span {
    let digest = Sha256::digest(principal.scope_id().as_bytes());
    let scope_id_hash = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    tracing::info_span!(
        "ram_a.tool",
        request_id = request_id,
        tool,
        scope_id_hash = scope_id_hash
    )
}

fn service_error(error: ServiceError) -> CallToolResult {
    CallToolResult::structured_error(serde_json::json!({
        "code": error.code(),
        "message": error.to_string(),
        "retriable": error.retriable(),
    }))
}

fn case_service_error(error: CaseServiceError) -> CallToolResult {
    CallToolResult::structured_error(serde_json::json!({
        "code": error.code(),
        "message": error.to_string(),
        "retriable": error.retriable(),
    }))
}

fn tool_error(code: &'static str, retriable: bool) -> CallToolResult {
    CallToolResult::structured_error(serde_json::json!({
        "code": code,
        "message": "memory tool request was rejected",
        "retriable": retriable,
    }))
}

impl ServerHandler for MemoryMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2025_11_25)
            .with_instructions(
                "Authenticated long-term memory and tenant-authorized case retrieval tools.",
            )
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(code) = self.disabled_tool_code(request.name.as_ref()) {
            return Ok(tool_error(code, false));
        }
        let context = ToolCallContext::new(self, request, context);
        self.tool_router.call(context).await
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: self.tool_router.list_all(),
            ..Default::default()
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_router.get(name).cloned()
    }
}

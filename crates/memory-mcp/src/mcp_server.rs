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
use tokio_util::sync::CancellationToken;

use crate::{
    CaseSearchRequest, CaseSearchResponse, CaseServiceError, DisabledCaseSearchProvider,
    DynCaseSearchProvider, FeatureFlags, IngestRequest, IngestResponse, MemoryService, Principal,
    SearchRequest, SearchResponse, ServiceError,
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
        let result = tokio::select! {
            biased;
            _ = self.cancellation_token.cancelled() => {
                return tool_error("CANCELLED", true);
            }
            result = self.service.ingest(principal, request) => result,
        };
        match result {
            Ok(response) => CallToolResult::structured(
                serde_json::to_value(response).expect("ingest response is serializable"),
            ),
            Err(error) => service_error(error),
        }
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
        let result = tokio::select! {
            biased;
            _ = self.cancellation_token.cancelled() => {
                return tool_error("CANCELLED", true);
            }
            result = self.service.search(principal, request) => result,
        };
        match result {
            Ok(response) => CallToolResult::structured(
                serde_json::to_value(response).expect("search response is serializable"),
            ),
            Err(error) => service_error(error),
        }
    }

    #[tool(
        name = "memory_case_search",
        description = "First use this tool for operational troubleshooting, incident diagnosis, case lookup, root-cause analysis, remediation steps, or similar historical case questions. It searches an authorized case library and returns evidence plus source references; base troubleshooting answers on those returned references instead of general knowledge alone.",
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
        let result = tokio::select! {
            biased;
            _ = self.cancellation_token.cancelled() => {
                return tool_error("CANCELLED", true);
            }
            result = self.case_search.search(principal, request) => result,
        };
        match result {
            Ok(response) => CallToolResult::structured(
                serde_json::to_value(response).expect("case search response is serializable"),
            ),
            Err(error) => case_service_error(error),
        }
    }
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

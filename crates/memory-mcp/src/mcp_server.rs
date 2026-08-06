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
    CaseDeleteProposalResponse, CaseDocumentDeleteRequest, CaseDocumentDeleteResponse,
    CaseDocumentMutationResponse, CaseDocumentUpdateRequest, CaseDocumentUploadRequest,
    CaseMutationConfirmationRequest, CaseMutationProposalResponse, CaseSearchRequest,
    CaseSearchResponse, CaseServiceError, DisabledCaseSearchProvider, DynCaseSearchProvider,
    FeatureFlags, IngestRequest, IngestResponse, MemoryService, Principal, SearchRequest,
    SearchResponse, ServiceError,
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
            tool_router.disable_route("memory_case_prepare_upload");
            tool_router.disable_route("memory_case_prepare_update");
            tool_router.disable_route("memory_case_prepare_delete");
            tool_router.disable_route("memory_case_upload");
            tool_router.disable_route("memory_case_update");
            tool_router.disable_route("memory_case_delete");
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
            "memory_case_search"
            | "memory_case_prepare_upload"
            | "memory_case_prepare_update"
            | "memory_case_prepare_delete"
            | "memory_case_upload"
            | "memory_case_update"
            | "memory_case_delete"
                if !self.features.case_library =>
            {
                Some("CASE_LIBRARY_DISABLED")
            }
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

    #[tool(
        name = "memory_case_prepare_upload",
        description = "After troubleshooting is complete, prepare—but do not write—a proposed new case document. Include the completed diagnosis summary and final case content. Present the returned proposal to the user, ask for explicit confirmation, end the turn, and do not call memory_case_upload until a later user message clearly confirms it.",
        output_schema = rmcp::handler::server::tool::schema_for_type::<CaseMutationProposalResponse>()
    )]
    async fn memory_case_prepare_upload(
        &self,
        Parameters(request): Parameters<CaseDocumentUploadRequest>,
        Extension(parts): Extension<axum::http::request::Parts>,
    ) -> CallToolResult {
        let Some(principal) = parts.extensions.get::<Principal>() else {
            return tool_error("UNAUTHORIZED", false);
        };
        if !principal
            .permissions
            .iter()
            .any(|permission| permission == "cases:write")
        {
            return tool_error("FORBIDDEN", false);
        }
        let result = tokio::select! {
            biased;
            _ = self.cancellation_token.cancelled() => {
                return tool_error("CANCELLED", true);
            }
            result = self.case_search.prepare_upload_document(principal, request) => result,
        };
        match result {
            Ok(response) => CallToolResult::structured(
                serde_json::to_value(response).expect("case upload proposal is serializable"),
            ),
            Err(error) => case_service_error(error),
        }
    }

    #[tool(
        name = "memory_case_upload",
        description = "Confirm and execute a previously prepared case upload. Call only after memory_case_prepare_upload, only in a later turn after the user explicitly confirms the displayed proposal, and pass that one-time confirmation_token with user_confirmed=true. Never infer confirmation from silence or from the original troubleshooting request.",
        output_schema = rmcp::handler::server::tool::schema_for_type::<CaseDocumentMutationResponse>()
    )]
    async fn memory_case_upload(
        &self,
        Parameters(request): Parameters<CaseMutationConfirmationRequest>,
        Extension(parts): Extension<axum::http::request::Parts>,
    ) -> CallToolResult {
        let Some(principal) = parts.extensions.get::<Principal>() else {
            return tool_error("UNAUTHORIZED", false);
        };
        if !principal
            .permissions
            .iter()
            .any(|permission| permission == "cases:write")
        {
            return tool_error("FORBIDDEN", false);
        }
        let result = tokio::select! {
            biased;
            _ = self.cancellation_token.cancelled() => {
                return tool_error("CANCELLED", true);
            }
            result = self.case_search.upload_document(principal, request) => result,
        };
        match result {
            Ok(response) => CallToolResult::structured(
                serde_json::to_value(response).expect("case upload response is serializable"),
            ),
            Err(error) => case_service_error(error),
        }
    }

    #[tool(
        name = "memory_case_prepare_update",
        description = "After troubleshooting is complete, prepare—but do not write—a replacement for an existing case document. Include the completed diagnosis summary and final replacement content. Present the returned proposal to the user, ask for explicit confirmation, end the turn, and do not call memory_case_update until a later user message clearly confirms it.",
        output_schema = rmcp::handler::server::tool::schema_for_type::<CaseMutationProposalResponse>()
    )]
    async fn memory_case_prepare_update(
        &self,
        Parameters(request): Parameters<CaseDocumentUpdateRequest>,
        Extension(parts): Extension<axum::http::request::Parts>,
    ) -> CallToolResult {
        let Some(principal) = parts.extensions.get::<Principal>() else {
            return tool_error("UNAUTHORIZED", false);
        };
        if !principal
            .permissions
            .iter()
            .any(|permission| permission == "cases:write")
        {
            return tool_error("FORBIDDEN", false);
        }
        let result = tokio::select! {
            biased;
            _ = self.cancellation_token.cancelled() => {
                return tool_error("CANCELLED", true);
            }
            result = self.case_search.prepare_update_document(principal, request) => result,
        };
        match result {
            Ok(response) => CallToolResult::structured(
                serde_json::to_value(response).expect("case update proposal is serializable"),
            ),
            Err(error) => case_service_error(error),
        }
    }

    #[tool(
        name = "memory_case_update",
        description = "Confirm and execute a previously prepared case update. Call only after memory_case_prepare_update, only in a later turn after the user explicitly confirms the displayed proposal, and pass that one-time confirmation_token with user_confirmed=true. Never infer confirmation from silence or from the original troubleshooting request.",
        output_schema = rmcp::handler::server::tool::schema_for_type::<CaseDocumentMutationResponse>()
    )]
    async fn memory_case_update(
        &self,
        Parameters(request): Parameters<CaseMutationConfirmationRequest>,
        Extension(parts): Extension<axum::http::request::Parts>,
    ) -> CallToolResult {
        let Some(principal) = parts.extensions.get::<Principal>() else {
            return tool_error("UNAUTHORIZED", false);
        };
        if !principal
            .permissions
            .iter()
            .any(|permission| permission == "cases:write")
        {
            return tool_error("FORBIDDEN", false);
        }
        let result = tokio::select! {
            biased;
            _ = self.cancellation_token.cancelled() => {
                return tool_error("CANCELLED", true);
            }
            result = self.case_search.update_document(principal, request) => result,
        };
        match result {
            Ok(response) => CallToolResult::structured(
                serde_json::to_value(response).expect("case update response is serializable"),
            ),
            Err(error) => case_service_error(error),
        }
    }

    #[tool(
        name = "memory_case_prepare_delete",
        description = "Prepare—but do not execute—the deletion of an existing case document. Use only after diagnosis is complete and there is a concrete reason to remove the case. Present the returned document identity and deletion reason to the user, ask for explicit confirmation, end the turn, and do not call memory_case_delete until a later user message clearly confirms it.",
        output_schema = rmcp::handler::server::tool::schema_for_type::<CaseDeleteProposalResponse>()
    )]
    async fn memory_case_prepare_delete(
        &self,
        Parameters(request): Parameters<CaseDocumentDeleteRequest>,
        Extension(parts): Extension<axum::http::request::Parts>,
    ) -> CallToolResult {
        let Some(principal) = parts.extensions.get::<Principal>() else {
            return tool_error("UNAUTHORIZED", false);
        };
        if !principal
            .permissions
            .iter()
            .any(|permission| permission == "cases:write")
        {
            return tool_error("FORBIDDEN", false);
        }
        let result = tokio::select! {
            biased;
            _ = self.cancellation_token.cancelled() => {
                return tool_error("CANCELLED", true);
            }
            result = self.case_search.prepare_delete_document(principal, request) => result,
        };
        match result {
            Ok(response) => CallToolResult::structured(
                serde_json::to_value(response).expect("case delete proposal is serializable"),
            ),
            Err(error) => case_service_error(error),
        }
    }

    #[tool(
        name = "memory_case_delete",
        description = "Confirm and execute a previously prepared case deletion. Call only after memory_case_prepare_delete, only in a later turn after the user explicitly confirms the displayed target and reason, and pass that one-time confirmation_token with user_confirmed=true. Deletion removes the document, source file, ingestion tasks, chunks, and search records. Never infer confirmation.",
        output_schema = rmcp::handler::server::tool::schema_for_type::<CaseDocumentDeleteResponse>()
    )]
    async fn memory_case_delete(
        &self,
        Parameters(request): Parameters<CaseMutationConfirmationRequest>,
        Extension(parts): Extension<axum::http::request::Parts>,
    ) -> CallToolResult {
        let Some(principal) = parts.extensions.get::<Principal>() else {
            return tool_error("UNAUTHORIZED", false);
        };
        if !principal
            .permissions
            .iter()
            .any(|permission| permission == "cases:write")
        {
            return tool_error("FORBIDDEN", false);
        }
        let result = tokio::select! {
            biased;
            _ = self.cancellation_token.cancelled() => {
                return tool_error("CANCELLED", true);
            }
            result = self.case_search.delete_document(principal, request) => result,
        };
        match result {
            Ok(response) => CallToolResult::structured(
                serde_json::to_value(response).expect("case delete response is serializable"),
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
                "Authenticated long-term memory plus tenant-authorized case tools. For case mutations, finish diagnosis first, call the matching memory_case_prepare_upload, memory_case_prepare_update, or memory_case_prepare_delete tool, show the returned proposal and ask the user to confirm, then end the turn. Only after a later user message explicitly confirms that proposal may you call the matching final tool with its one-time token and user_confirmed=true. Never treat silence, the original request, or an ambiguous reply as confirmation.",
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

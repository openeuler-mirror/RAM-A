use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use rmcp::transport::streamable_http_server::{
    session::{local::LocalSessionManager, SessionId, SessionManager},
    StreamableHttpServerConfig, StreamableHttpService,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::{FeatureFlags, HttpConfig, LimitsConfig};
use crate::mcp_server::{DynMemoryService, MemoryMcpServer};
use crate::{DisabledCaseSearchProvider, DynCaseSearchProvider, Principal, TokenAuthenticator};

pub const AGENT_ID_HEADER: &str = "x-agent-id";
pub const REQUEST_ID_HEADER: &str = "x-request-id";
const MCP_PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const LIMIT_REASON_HEADER: &str = "x-ram-a-limit-reason";

pub struct HttpRuntime {
    service: DynMemoryService,
    case_search: DynCaseSearchProvider,
    authenticator: Arc<TokenAuthenticator>,
    database_path: PathBuf,
    providers_ready: bool,
    features: FeatureFlags,
    cancellation_token: CancellationToken,
}

impl HttpRuntime {
    pub fn new(
        service: DynMemoryService,
        authenticator: Arc<TokenAuthenticator>,
        database_path: PathBuf,
        providers_ready: bool,
    ) -> Self {
        Self::with_cancellation_token(
            service,
            authenticator,
            database_path,
            providers_ready,
            CancellationToken::new(),
        )
    }

    pub fn with_cancellation_token(
        service: DynMemoryService,
        authenticator: Arc<TokenAuthenticator>,
        database_path: PathBuf,
        providers_ready: bool,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            service,
            case_search: Arc::new(DisabledCaseSearchProvider),
            authenticator,
            database_path,
            providers_ready,
            features: FeatureFlags::all(),
            cancellation_token,
        }
    }

    pub fn with_case_search_provider(mut self, case_search: DynCaseSearchProvider) -> Self {
        self.case_search = case_search;
        self
    }

    pub fn with_features(mut self, features: FeatureFlags) -> Self {
        self.features = features;
        self
    }
}

#[derive(Clone)]
struct McpAuthState {
    authenticator: Arc<TokenAuthenticator>,
    allowed_origins: Arc<HashSet<String>>,
    max_body_bytes: usize,
    rate_limiter: Arc<DefaultKeyedRateLimiter<String>>,
    semaphores: Arc<Mutex<HashMap<String, Weak<Semaphore>>>>,
    max_in_flight: usize,
    initialize_rate_limiter: Arc<DefaultKeyedRateLimiter<PrincipalKey>>,
    session_admission: Arc<SessionAdmission>,
    session_manager: Arc<LocalSessionManager>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PrincipalKey {
    scope_id: String,
    agent_id: String,
}

impl PrincipalKey {
    fn new(principal: &Principal) -> Self {
        Self {
            scope_id: principal.scope_id(),
            agent_id: principal.agent_id.clone(),
        }
    }
}

struct ActiveSession {
    owner: PrincipalKey,
    last_seen: tokio::time::Instant,
}

#[derive(Default)]
struct SessionAdmissionInner {
    active: HashMap<SessionId, ActiveSession>,
    closing: HashSet<SessionId>,
    pending_by_principal: HashMap<PrincipalKey, usize>,
    pending_global: usize,
}

struct SessionAdmission {
    inner: Mutex<SessionAdmissionInner>,
    max_per_principal: usize,
    max_global: usize,
    idle_timeout: Duration,
}

impl SessionAdmission {
    fn new(max_per_principal: usize, max_global: usize, idle_timeout: Duration) -> Self {
        Self {
            inner: Mutex::new(SessionAdmissionInner::default()),
            max_per_principal,
            max_global,
            idle_timeout,
        }
    }

    fn reserve(self: &Arc<Self>, principal: PrincipalKey) -> AdmissionAttempt {
        let now = tokio::time::Instant::now();
        let mut inner = self.inner.lock().expect("session admission lock poisoned");
        let mut expired = Vec::new();
        inner.active.retain(|session_id, session| {
            let keep = now.duration_since(session.last_seen) < self.idle_timeout;
            if !keep {
                expired.push(session_id.clone());
            }
            keep
        });
        inner.closing.extend(expired.iter().cloned());

        let active_for_principal = inner
            .active
            .values()
            .filter(|session| session.owner == principal)
            .count();
        let pending_for_principal = inner
            .pending_by_principal
            .get(&principal)
            .copied()
            .unwrap_or_default();
        let at_principal_cap =
            active_for_principal + pending_for_principal >= self.max_per_principal;
        let at_global_cap = inner.active.len() + inner.pending_global >= self.max_global;
        let reservation = if at_principal_cap || at_global_cap {
            None
        } else {
            inner.pending_global += 1;
            *inner
                .pending_by_principal
                .entry(principal.clone())
                .or_default() += 1;
            Some(PendingSessionReservation {
                admission: self.clone(),
                principal,
                pending: true,
            })
        };
        AdmissionAttempt {
            reservation,
            expired,
        }
    }

    fn release_pending(&self, principal: &PrincipalKey) {
        let mut inner = self.inner.lock().expect("session admission lock poisoned");
        inner.pending_global = inner.pending_global.saturating_sub(1);
        if let Some(count) = inner.pending_by_principal.get_mut(principal) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                inner.pending_by_principal.remove(principal);
            }
        }
    }

    fn commit_pending(&self, principal: &PrincipalKey, session_id: SessionId) {
        let mut inner = self.inner.lock().expect("session admission lock poisoned");
        inner.pending_global = inner.pending_global.saturating_sub(1);
        if let Some(count) = inner.pending_by_principal.get_mut(principal) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                inner.pending_by_principal.remove(principal);
            }
        }
        inner.closing.remove(&session_id);
        inner.active.insert(
            session_id,
            ActiveSession {
                owner: principal.clone(),
                last_seen: tokio::time::Instant::now(),
            },
        );
    }

    fn validate_and_touch(
        &self,
        session_id: &SessionId,
        principal: &PrincipalKey,
    ) -> SessionAccess {
        let now = tokio::time::Instant::now();
        let mut inner = self.inner.lock().expect("session admission lock poisoned");
        if inner.closing.contains(session_id) {
            return SessionAccess::Closing;
        }
        let Some(session) = inner.active.get_mut(session_id) else {
            return SessionAccess::Unknown;
        };
        if now.duration_since(session.last_seen) >= self.idle_timeout {
            inner.active.remove(session_id);
            inner.closing.insert(session_id.clone());
            return SessionAccess::Expired;
        }
        if session.owner != *principal {
            return SessionAccess::Forbidden;
        }
        session.last_seen = now;
        SessionAccess::Allowed
    }

    fn mark_closing(&self, session_id: &SessionId) {
        let mut inner = self.inner.lock().expect("session admission lock poisoned");
        inner.active.remove(session_id);
        inner.closing.insert(session_id.clone());
    }

    fn finish_closing(&self, session_id: &SessionId) {
        self.inner
            .lock()
            .expect("session admission lock poisoned")
            .closing
            .remove(session_id);
    }
}

struct AdmissionAttempt {
    reservation: Option<PendingSessionReservation>,
    expired: Vec<SessionId>,
}

struct PendingSessionReservation {
    admission: Arc<SessionAdmission>,
    principal: PrincipalKey,
    pending: bool,
}

impl PendingSessionReservation {
    fn commit(mut self, session_id: SessionId) {
        self.admission.commit_pending(&self.principal, session_id);
        self.pending = false;
    }
}

impl Drop for PendingSessionReservation {
    fn drop(&mut self) {
        if self.pending {
            self.admission.release_pending(&self.principal);
        }
    }
}

enum SessionAccess {
    Allowed,
    Unknown,
    Closing,
    Forbidden,
    Expired,
}

pub fn create_http_router(
    runtime: HttpRuntime,
    http: &HttpConfig,
    limits: &LimitsConfig,
) -> Router {
    let readiness = ReadinessState {
        database_path: runtime.database_path.clone(),
        dependencies_constructed: runtime.providers_ready,
        queue_capacity: limits.max_in_flight_per_principal_tool,
    };
    let service_state = runtime.service;
    let case_search = runtime.case_search;
    let features = runtime.features;
    let cancellation_token = runtime.cancellation_token.clone();
    let service_cancellation_token = cancellation_token.clone();
    let session_manager = Arc::new(LocalSessionManager::default());
    let mcp_service: StreamableHttpService<MemoryMcpServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || {
                Ok(MemoryMcpServer::with_case_search_provider(
                    service_state.clone(),
                    service_cancellation_token.clone(),
                    case_search.clone(),
                    features,
                ))
            },
            session_manager.clone(),
            StreamableHttpServerConfig::default()
                .with_allowed_hosts(http.allowed_hosts.clone())
                .with_cancellation_token(cancellation_token)
                .with_sse_keep_alive(None),
        );
    let auth = McpAuthState {
        authenticator: runtime.authenticator,
        allowed_origins: Arc::new(http.allowed_origins.iter().cloned().collect()),
        max_body_bytes: limits.max_body_bytes,
        rate_limiter: Arc::new(RateLimiter::keyed(
            Quota::per_second(
                NonZeroU32::new(limits.requests_per_second.max(1))
                    .expect("clamped rate is non-zero"),
            )
            .allow_burst(
                NonZeroU32::new(limits.rate_burst.max(1)).expect("clamped burst is non-zero"),
            ),
        )),
        semaphores: Arc::new(Mutex::new(HashMap::new())),
        max_in_flight: limits.max_in_flight_per_principal_tool.max(1),
        initialize_rate_limiter: Arc::new(RateLimiter::keyed(
            Quota::per_second(
                NonZeroU32::new(limits.initialize_requests_per_second.max(1))
                    .expect("clamped rate is non-zero"),
            )
            .allow_burst(
                NonZeroU32::new(limits.initialize_rate_burst.max(1))
                    .expect("clamped burst is non-zero"),
            ),
        )),
        session_admission: Arc::new(SessionAdmission::new(
            limits.max_active_sessions_per_principal.max(1),
            limits.max_active_sessions_global.max(1),
            Duration::from_secs(limits.session_idle_timeout_seconds.max(1)),
        )),
        session_manager,
    };
    let mcp = Router::new()
        .route_service("/mcp", mcp_service)
        .layer(middleware::from_fn_with_state(auth, authorize_mcp));

    let public = Router::new()
        .route("/healthy", get(|| async { StatusCode::OK }))
        .route("/ready", get(readiness_status))
        .with_state(readiness);
    public.merge(mcp)
}

#[derive(Clone)]
struct ReadinessState {
    database_path: PathBuf,
    dependencies_constructed: bool,
    queue_capacity: usize,
}

async fn readiness_status(State(state): State<ReadinessState>) -> Response {
    if !state.dependencies_constructed || state.queue_capacity == 0 {
        return (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response();
    }
    let database_path = state.database_path;
    let sqlite_ready = tokio::task::spawn_blocking(move || {
        let connection = rusqlite::Connection::open_with_flags(
            database_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .ok()?;
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table'
                   AND name IN ('memories', 'memory_fts', 'mcp_ingest_idempotency')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .ok()
            .filter(|count| *count == 3)
    })
    .await
    .ok()
    .flatten()
    .is_some();
    if sqlite_ready {
        (StatusCode::OK, "ready").into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response()
    }
}

async fn authorize_mcp(
    State(state): State<McpAuthState>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_id = Uuid::new_v4().to_string();
    let method_is_delete = request.method() == axum::http::Method::DELETE;
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        let allowed = origin
            .to_str()
            .ok()
            .is_some_and(|origin| state.allowed_origins.contains(origin));
        if !allowed {
            return error_response(StatusCode::FORBIDDEN, "forbidden", &request_id);
        }
    }

    let Some(token) = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|token| !token.is_empty())
    else {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", &request_id);
    };
    let client_agent_id = match request.headers().get(AGENT_ID_HEADER) {
        Some(value) => match value.to_str() {
            Ok(value) => Some(value),
            Err(_) => return error_response(StatusCode::FORBIDDEN, "forbidden", &request_id),
        },
        None => None,
    };
    let Ok(principal) = state.authenticator.authenticate(token) else {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", &request_id);
    };
    if client_agent_id.is_some_and(|agent_id| agent_id != principal.agent_id) {
        return error_response(StatusCode::FORBIDDEN, "forbidden", &request_id);
    }
    let requested_operation = match requested_operation(&mut request, state.max_body_bytes).await {
        Ok(operation) => operation,
        Err(()) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload too large",
                &request_id,
            )
        }
    };
    if matches!(requested_operation, RequestedOperation::UnsupportedProtocol) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "unsupported protocol version",
            &request_id,
        );
    }
    let principal_key = PrincipalKey::new(&principal);
    let mut reservation = None;
    if matches!(requested_operation, RequestedOperation::Initialize) {
        if state
            .initialize_rate_limiter
            .check_key(&principal_key)
            .is_err()
        {
            return limit_response(LimitReason::InitializeRate, &request_id);
        }
        let attempt = state.session_admission.reserve(principal_key.clone());
        close_sessions(
            &state.session_manager,
            &state.session_admission,
            attempt.expired,
        )
        .await;
        let Some(pending) = attempt.reservation else {
            return limit_response(LimitReason::SessionAdmission, &request_id);
        };
        reservation = Some(pending);
    }

    let session_id = request_session_id(&request);
    if let Some(session_id) = session_id.as_ref() {
        match state
            .session_admission
            .validate_and_touch(session_id, &principal_key)
        {
            SessionAccess::Allowed => {}
            SessionAccess::Forbidden => {
                return error_response(StatusCode::FORBIDDEN, "forbidden", &request_id)
            }
            SessionAccess::Expired => {
                close_sessions(
                    &state.session_manager,
                    &state.session_admission,
                    vec![session_id.clone()],
                )
                .await;
                return error_response(StatusCode::NOT_FOUND, "session not found", &request_id);
            }
            SessionAccess::Unknown | SessionAccess::Closing => {
                return error_response(StatusCode::NOT_FOUND, "session not found", &request_id)
            }
        }
        if request
            .headers()
            .get(MCP_PROTOCOL_VERSION_HEADER)
            .and_then(|value| value.to_str().ok())
            != Some(MCP_PROTOCOL_VERSION)
        {
            return error_response(
                StatusCode::BAD_REQUEST,
                "unsupported protocol version",
                &request_id,
            );
        }
    }

    if method_is_delete {
        if let Some(session_id) = session_id.as_ref() {
            state.session_admission.mark_closing(session_id);
        }
    }

    let concurrency_permit = if let RequestedOperation::Tool(tool) = requested_operation {
        if !principal
            .permissions
            .iter()
            .any(|permission| permission == tool.permission())
        {
            return error_response(StatusCode::FORBIDDEN, "forbidden", &request_id);
        }
        let rate_key = format!(
            "{}\0{}\0{}",
            principal.scope_id(),
            principal.agent_id,
            tool.name()
        );
        if state.rate_limiter.check_key(&rate_key).is_err() {
            return limit_response(LimitReason::ToolRate, &request_id);
        }
        match try_acquire_tool(&state, &rate_key) {
            Some(permit) => Some(permit),
            None => return limit_response(LimitReason::ToolConcurrency, &request_id),
        }
    } else {
        None
    };
    if let Some(permit) = &concurrency_permit {
        request.extensions_mut().insert(permit.clone());
    }
    request.extensions_mut().insert(principal);
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));
    let mut response = next.run(request).await;
    if let Some(pending) = reservation {
        if response.status().is_success() {
            if let Some(session_id) = response_session_id(&response) {
                pending.commit(session_id);
            }
        }
    }
    if method_is_delete {
        if let Some(session_id) = session_id {
            let _ = state.session_manager.close_session(&session_id).await;
            state.session_admission.finish_closing(&session_id);
        }
    }
    response.headers_mut().insert(
        REQUEST_ID_HEADER,
        HeaderValue::from_str(&request_id).expect("UUID is a valid header value"),
    );
    response
}

fn request_session_id(request: &Request) -> Option<SessionId> {
    request
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(Arc::<str>::from)
}

fn response_session_id(response: &Response) -> Option<SessionId> {
    response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(Arc::<str>::from)
}

async fn close_sessions(
    manager: &LocalSessionManager,
    admission: &SessionAdmission,
    session_ids: Vec<SessionId>,
) {
    for session_id in session_ids {
        let _ = manager.close_session(&session_id).await;
        admission.finish_closing(&session_id);
    }
}

struct ConcurrencyPermit {
    _permit: OwnedSemaphorePermit,
}

fn try_acquire_tool(state: &McpAuthState, key: &str) -> Option<Arc<ConcurrencyPermit>> {
    let semaphore = {
        let mut semaphores = state.semaphores.lock().ok()?;
        semaphores.retain(|_, semaphore| semaphore.strong_count() > 0);
        if let Some(semaphore) = semaphores.get(key).and_then(Weak::upgrade) {
            semaphore
        } else {
            let semaphore = Arc::new(Semaphore::new(state.max_in_flight));
            semaphores.insert(key.to_string(), Arc::downgrade(&semaphore));
            semaphore
        }
    };
    semaphore
        .try_acquire_owned()
        .ok()
        .map(|permit| Arc::new(ConcurrencyPermit { _permit: permit }))
}

#[derive(Clone, Copy)]
enum RequestedTool {
    Ingest,
    Search,
    CaseSearch,
    CasePrepareUpload,
    CaseUpload,
    CasePrepareUpdate,
    CaseUpdate,
    CasePrepareDelete,
    CaseDelete,
}

#[derive(Clone, Copy)]
enum RequestedOperation {
    Initialize,
    UnsupportedProtocol,
    Tool(RequestedTool),
    Other,
}

impl RequestedTool {
    fn name(self) -> &'static str {
        match self {
            Self::Ingest => "memory_ingest",
            Self::Search => "memory_search",
            Self::CaseSearch => "memory_case_search",
            Self::CasePrepareUpload => "memory_case_prepare_upload",
            Self::CaseUpload => "memory_case_upload",
            Self::CasePrepareUpdate => "memory_case_prepare_update",
            Self::CaseUpdate => "memory_case_update",
            Self::CasePrepareDelete => "memory_case_prepare_delete",
            Self::CaseDelete => "memory_case_delete",
        }
    }

    fn permission(self) -> &'static str {
        match self {
            Self::Ingest => "memory:write",
            Self::Search => "memory:read",
            Self::CaseSearch => "cases:read",
            Self::CasePrepareUpload
            | Self::CaseUpload
            | Self::CasePrepareUpdate
            | Self::CaseUpdate
            | Self::CasePrepareDelete
            | Self::CaseDelete => "cases:write",
        }
    }
}

async fn requested_operation(
    request: &mut Request,
    max_body_bytes: usize,
) -> Result<RequestedOperation, ()> {
    let body = std::mem::replace(request.body_mut(), Body::empty());
    let bytes = to_bytes(body, max_body_bytes).await.map_err(|_| ())?;
    *request.body_mut() = Body::from(bytes.clone());
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Ok(RequestedOperation::Other);
    };
    let method = value.get("method").and_then(serde_json::Value::as_str);
    if method == Some("initialize") {
        return Ok(
            if value
                .pointer("/params/protocolVersion")
                .and_then(serde_json::Value::as_str)
                == Some(MCP_PROTOCOL_VERSION)
            {
                RequestedOperation::Initialize
            } else {
                RequestedOperation::UnsupportedProtocol
            },
        );
    }
    if method != Some("tools/call") {
        return Ok(RequestedOperation::Other);
    }
    Ok(
        match value
            .pointer("/params/name")
            .and_then(serde_json::Value::as_str)
        {
            Some("memory_ingest") => RequestedOperation::Tool(RequestedTool::Ingest),
            Some("memory_search") => RequestedOperation::Tool(RequestedTool::Search),
            Some("memory_case_search") => RequestedOperation::Tool(RequestedTool::CaseSearch),
            Some("memory_case_prepare_upload") => {
                RequestedOperation::Tool(RequestedTool::CasePrepareUpload)
            }
            Some("memory_case_upload") => RequestedOperation::Tool(RequestedTool::CaseUpload),
            Some("memory_case_prepare_update") => {
                RequestedOperation::Tool(RequestedTool::CasePrepareUpdate)
            }
            Some("memory_case_update") => RequestedOperation::Tool(RequestedTool::CaseUpdate),
            Some("memory_case_prepare_delete") => {
                RequestedOperation::Tool(RequestedTool::CasePrepareDelete)
            }
            Some("memory_case_delete") => RequestedOperation::Tool(RequestedTool::CaseDelete),
            _ => RequestedOperation::Other,
        },
    )
}

#[derive(Clone, Debug)]
pub struct RequestId(pub String);

#[derive(Clone, Copy)]
enum LimitReason {
    InitializeRate,
    SessionAdmission,
    ToolRate,
    ToolConcurrency,
}

impl LimitReason {
    fn as_header_value(self) -> &'static str {
        match self {
            Self::InitializeRate => "initialize_rate_limit",
            Self::SessionAdmission => "session_admission",
            Self::ToolRate => "tool_rate_limit",
            Self::ToolConcurrency => "tool_concurrency",
        }
    }
}

fn limit_response(reason: LimitReason, request_id: &str) -> Response {
    let mut response = error_response(
        StatusCode::TOO_MANY_REQUESTS,
        "too many requests",
        request_id,
    );
    response.headers_mut().insert(
        LIMIT_REASON_HEADER,
        HeaderValue::from_static(reason.as_header_value()),
    );
    response
}

fn error_response(status: StatusCode, message: &'static str, request_id: &str) -> Response {
    let mut response = (status, message).into_response();
    if status == StatusCode::TOO_MANY_REQUESTS {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    }
    response.headers_mut().insert(
        REQUEST_ID_HEADER,
        HeaderValue::from_str(request_id).expect("UUID is a valid header value"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn expired_session_is_tombstoned_before_async_close() {
        let admission = Arc::new(SessionAdmission::new(2, 2, Duration::from_secs(1)));
        let owner = PrincipalKey {
            scope_id: "scope-a".to_string(),
            agent_id: "agent-a".to_string(),
        };
        let session_id: SessionId = Arc::from("expiring-session");
        admission
            .reserve(owner.clone())
            .reservation
            .unwrap()
            .commit(session_id.clone());
        tokio::time::advance(Duration::from_secs(2)).await;

        let attempt = admission.reserve(owner.clone());
        assert_eq!(
            attempt.expired.as_slice(),
            std::slice::from_ref(&session_id)
        );
        let admission_for_request = admission.clone();
        let session_for_request = session_id.clone();
        let access = tokio::spawn(async move {
            admission_for_request.validate_and_touch(&session_for_request, &owner)
        })
        .await
        .unwrap();

        assert!(matches!(access, SessionAccess::Closing));
    }
}

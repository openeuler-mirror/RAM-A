use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use axum::response::Response;
use axum::Router;
use memory_cases::{build_service, CaseServiceOptions};
use memory_core::{HashEmbedding, MemoryManager, SqliteMemoryStore};
use memory_mcp::{
    create_http_router, AuthConfig, CaseLibraryConfig, DynCaseSearchProvider,
    EmbeddedCaseSearchProvider, EmbeddingProviderKind, FeatureFlags, GraphMemoryRetrievalConfig,
    HttpConfig, HttpRuntime, IdempotencyRepository, LimitsConfig, MemoryService, ProvidersConfig,
    ServerConfig, StorageConfig, TokenAuthenticator, TokenConfig,
};
use memory_pipeline::error::Result as PipelineResult;
use memory_pipeline::extraction::{ExtractionBatch, MemoryExtractor, ModelUsage, SCHEMA_VERSION};
use memory_pipeline::grounding::{GroundingBatch, GroundingResult, GroundingVerifier};
use memory_pipeline::models::{AtomicMemory, ExtractionWindow, NormalizedMessage};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "alice-test-token";
const BOB_TOKEN: &str = "bob-test-token";

struct PreferenceExtractor {
    started: Arc<tokio::sync::Notify>,
    dropped: Arc<tokio::sync::Notify>,
    delay: Duration,
}

struct DropNotifier(Arc<tokio::sync::Notify>);

impl Drop for DropNotifier {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

#[async_trait]
impl MemoryExtractor for PreferenceExtractor {
    fn model(&self) -> &str {
        "fixture"
    }

    fn prompt_version(&self) -> &str {
        "fixture-v1"
    }

    fn implementation(&self) -> &'static str {
        "PreferenceExtractor"
    }

    async fn extract(
        &self,
        window: &ExtractionWindow,
        messages: &HashMap<String, NormalizedMessage>,
    ) -> PipelineResult<ExtractionBatch> {
        let _drop_notifier = DropNotifier(self.dropped.clone());
        self.started.notify_one();
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        let candidate = &window.candidate_refs[0];
        let message = &messages[&candidate.message_id];
        Ok(ExtractionBatch {
            window_id: window.id.clone(),
            schema_version: SCHEMA_VERSION.to_string(),
            raw_memories: vec![json!({
                "text": message.text,
                "memory_type": "preference",
                "subject": {"name": "user"},
                "predicate": "prefers",
                "object": "window seat",
                "modality": "asserted",
                "event_time": null,
                "attributes": {},
                "evidence": [{
                    "message_id": candidate.message_id,
                    "quote": candidate.text,
                    "evidence_role": "primary"
                }]
            })],
            usage: ModelUsage::default(),
            raw_response: String::new(),
        })
    }
}

struct SupportingVerifier;

#[async_trait]
impl GroundingVerifier for SupportingVerifier {
    fn model(&self) -> &str {
        "fixture"
    }

    fn prompt_version(&self) -> &str {
        "fixture-v1"
    }

    fn implementation(&self) -> &'static str {
        "SupportingVerifier"
    }

    async fn verify(
        &self,
        window: &ExtractionWindow,
        memories: &[AtomicMemory],
        _messages: &HashMap<String, NormalizedMessage>,
    ) -> PipelineResult<GroundingBatch> {
        Ok(GroundingBatch {
            window_id: window.id.clone(),
            results: memories
                .iter()
                .map(|memory| GroundingResult {
                    memory_id: memory.id.clone(),
                    status: "SUPPORTED".to_string(),
                    reason: String::new(),
                })
                .collect(),
            usage: ModelUsage::default(),
            raw_response: String::new(),
        })
    }
}

struct Fixture {
    app: Router,
    extract_started: Arc<tokio::sync::Notify>,
    extract_dropped: Arc<tokio::sync::Notify>,
    cancellation_token: tokio_util::sync::CancellationToken,
    database_path: std::path::PathBuf,
    _temp: TempDir,
}

async fn fixture_router() -> Fixture {
    fixture_router_with_options(
        &["memory:read", "memory:write"],
        LimitsConfig::default(),
        Duration::ZERO,
    )
    .await
}

async fn fixture_router_with_permissions(permissions: &[&str], limits: LimitsConfig) -> Fixture {
    fixture_router_with_options(permissions, limits, Duration::ZERO).await
}

async fn fixture_router_with_options(
    permissions: &[&str],
    limits: LimitsConfig,
    extraction_delay: Duration,
) -> Fixture {
    fixture_router_full(permissions, limits, extraction_delay, true).await
}

async fn fixture_router_full(
    permissions: &[&str],
    limits: LimitsConfig,
    extraction_delay: Duration,
    providers_ready: bool,
) -> Fixture {
    fixture_router_with_schema(permissions, limits, extraction_delay, providers_ready, true).await
}

async fn fixture_router_with_schema(
    permissions: &[&str],
    limits: LimitsConfig,
    extraction_delay: Duration,
    providers_ready: bool,
    initialize_memory_schema: bool,
) -> Fixture {
    fixture_router_with_schema_and_features(
        permissions,
        limits,
        extraction_delay,
        providers_ready,
        initialize_memory_schema,
        FeatureFlags::all(),
        None,
    )
    .await
}

async fn fixture_router_with_features(features: FeatureFlags) -> Fixture {
    fixture_router_with_schema_and_features(
        &["memory:read", "memory:write", "cases:read"],
        LimitsConfig::default(),
        Duration::ZERO,
        true,
        true,
        features,
        None,
    )
    .await
}

async fn fixture_router_with_schema_and_features(
    permissions: &[&str],
    limits: LimitsConfig,
    extraction_delay: Duration,
    providers_ready: bool,
    initialize_memory_schema: bool,
    features: FeatureFlags,
    case_search: Option<DynCaseSearchProvider>,
) -> Fixture {
    std::env::set_var("RAM_A_HTTP_MCP_TEST_TOKEN", TOKEN);
    std::env::set_var("RAM_A_HTTP_MCP_BOB_TEST_TOKEN", BOB_TOKEN);
    let temp = tempfile::tempdir().unwrap();
    let database_path = temp.path().join("memory.sqlite");
    let memory_store = Arc::new(SqliteMemoryStore::new(&database_path));
    if initialize_memory_schema {
        memory_store.initialize().await.unwrap();
    }
    let manager = Arc::new(MemoryManager::new(
        memory_store,
        Arc::new(HashEmbedding::new(32)),
    ));
    let idempotency = IdempotencyRepository::open(&database_path).await.unwrap();
    let extract_started = Arc::new(tokio::sync::Notify::new());
    let extract_dropped = Arc::new(tokio::sync::Notify::new());
    let extractor: Arc<dyn MemoryExtractor> = Arc::new(PreferenceExtractor {
        started: extract_started.clone(),
        dropped: extract_dropped.clone(),
        delay: extraction_delay,
    });
    let verifier: Arc<dyn GroundingVerifier> = Arc::new(SupportingVerifier);
    let service = MemoryService::new(manager, idempotency, extractor, verifier);
    let authenticator = TokenAuthenticator::from_config(&AuthConfig {
        tokens: vec![
            TokenConfig {
                token_env: "RAM_A_HTTP_MCP_TEST_TOKEN".to_string(),
                tenant_id: "tenant-a".to_string(),
                user_id: "alice".to_string(),
                agent_id: "agent-a".to_string(),
                permissions: permissions
                    .iter()
                    .map(|permission| (*permission).to_string())
                    .collect(),
            },
            TokenConfig {
                token_env: "RAM_A_HTTP_MCP_BOB_TEST_TOKEN".to_string(),
                tenant_id: "tenant-a".to_string(),
                user_id: "bob".to_string(),
                agent_id: "agent-b".to_string(),
                permissions: permissions
                    .iter()
                    .map(|permission| (*permission).to_string())
                    .collect(),
            },
        ],
    })
    .unwrap();
    let http = HttpConfig {
        allowed_origins: vec!["https://allowed.example".to_string()],
        allowed_hosts: vec!["memory.example.test".to_string()],
        ..HttpConfig::default()
    };
    let cancellation_token = tokio_util::sync::CancellationToken::new();
    let mut runtime = HttpRuntime::with_cancellation_token(
        service,
        Arc::new(authenticator),
        database_path.clone(),
        providers_ready,
        cancellation_token.clone(),
    )
    .with_features(features);
    if let Some(case_search) = case_search {
        runtime = runtime.with_case_search_provider(case_search);
    }
    Fixture {
        app: create_http_router(runtime, &http, &limits),
        extract_started,
        extract_dropped,
        cancellation_token,
        database_path,
        _temp: temp,
    }
}

fn initialize_request(token: Option<&str>, origin: Option<&str>) -> Request<Body> {
    initialize_request_with_version(token, origin, "2025-11-25")
}

fn initialize_request_with_version(
    token: Option<&str>,
    origin: Option<&str>,
    protocol_version: &str,
) -> Request<Body> {
    mcp_request(
        token,
        origin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": protocol_version,
                "capabilities": {},
                "clientInfo": {"name": "memory-mcp-test", "version": "1"}
            }
        }),
    )
}

fn mcp_request(token: Option<&str>, origin: Option<&str>, body: Value) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::HOST, "memory.example.test")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, token);
    }
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

fn session_request(session_id: &str, body: Value) -> Request<Body> {
    session_request_with_token(TOKEN, session_id, body)
}

fn session_request_with_token(token: &str, session_id: &str, body: Value) -> Request<Body> {
    let mut request = mcp_request(Some(&format!("Bearer {token}")), None, body);
    request
        .headers_mut()
        .insert("mcp-session-id", session_id.parse().unwrap());
    request
        .headers_mut()
        .insert("mcp-protocol-version", "2025-11-25".parse().unwrap());
    request
}

fn delete_session_request(token: &str, session_id: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri("/mcp")
        .header(header::HOST, "memory.example.test")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("mcp-session-id", session_id)
        .header("mcp-protocol-version", "2025-11-25")
        .body(Body::empty())
        .unwrap()
}

async fn response_json(response: Response) -> Value {
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    if let Some(data) = body
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .find(|data| !data.trim().is_empty())
    {
        serde_json::from_str(data.trim()).unwrap()
    } else {
        serde_json::from_str(&body).unwrap()
    }
}

async fn initialize(app: &Router) -> (String, Value) {
    initialize_with_token(app, TOKEN).await
}

async fn initialize_with_token(app: &Router, token: &str) -> (String, Value) {
    let response = app
        .clone()
        .oneshot(initialize_request(Some(&format!("Bearer {token}")), None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let session_id = response
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    (session_id, response_json(response).await)
}

async fn call_tool(
    app: &Router,
    session_id: &str,
    id: u64,
    name: &str,
    arguments: Value,
) -> Response {
    call_tool_with_token(app, TOKEN, session_id, id, name, arguments).await
}

async fn call_tool_with_token(
    app: &Router,
    token: &str,
    session_id: &str,
    id: u64,
    name: &str,
    arguments: Value,
) -> Response {
    app.clone()
        .oneshot(session_request_with_token(
            token,
            session_id,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments}
            }),
        ))
        .await
        .unwrap()
}

#[tokio::test]
async fn mcp_requires_bearer_and_rejects_disallowed_origin() {
    let fixture = fixture_router().await;
    let unauthorized = fixture
        .app
        .clone()
        .oneshot(initialize_request(None, None))
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    for method in ["GET", "DELETE"] {
        let request = Request::builder()
            .method(method)
            .uri("/mcp")
            .header(header::HOST, "memory.example.test")
            .body(Body::empty())
            .unwrap();
        let response = fixture.app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    let forbidden = fixture
        .app
        .oneshot(initialize_request(
            Some(&format!("Bearer {TOKEN}")),
            Some("https://evil.example"),
        ))
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn mcp_rejects_a_host_outside_the_configured_allowlist() {
    let fixture = fixture_router().await;
    let mut request = initialize_request(Some(&format!("Bearer {TOKEN}")), None);
    request
        .headers_mut()
        .insert(header::HOST, "attacker.example".parse().unwrap());
    let response = fixture.app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn mcp_endpoint_rejects_trailing_slash_and_subpaths() {
    let fixture = fixture_router().await;
    for path in ["/mcp/", "/mcp/extra"] {
        let mut request = initialize_request(Some(&format!("Bearer {TOKEN}")), None);
        *request.uri_mut() = path.parse().unwrap();
        let response = fixture.app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn active_session_cap_is_enforced_per_principal() {
    let limits = LimitsConfig {
        initialize_requests_per_second: 100,
        initialize_rate_burst: 100,
        max_active_sessions_per_principal: 1,
        max_active_sessions_global: 10,
        ..LimitsConfig::default()
    };
    let fixture = fixture_router_with_permissions(&["memory:read"], limits).await;
    let _ = initialize(&fixture.app).await;

    let rejected = fixture
        .app
        .oneshot(initialize_request(Some(&format!("Bearer {TOKEN}")), None))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(rejected.headers().get(header::RETRY_AFTER).unwrap(), "1");
    assert_eq!(
        rejected.headers().get("x-ram-a-limit-reason").unwrap(),
        "session_admission"
    );
    assert_eq!(
        to_bytes(rejected.into_body(), 1024).await.unwrap().as_ref(),
        b"too many requests"
    );
}

#[tokio::test]
async fn per_principal_session_cap_does_not_consume_another_principals_slot() {
    let limits = LimitsConfig {
        initialize_requests_per_second: 100,
        initialize_rate_burst: 100,
        max_active_sessions_per_principal: 1,
        max_active_sessions_global: 2,
        ..LimitsConfig::default()
    };
    let fixture = fixture_router_with_permissions(&["memory:read"], limits).await;
    let _ = initialize(&fixture.app).await;

    let alice_second = fixture
        .app
        .clone()
        .oneshot(initialize_request(Some(&format!("Bearer {TOKEN}")), None))
        .await
        .unwrap();
    assert_eq!(alice_second.status(), StatusCode::TOO_MANY_REQUESTS);

    let _ = initialize_with_token(&fixture.app, BOB_TOKEN).await;
}

#[tokio::test]
async fn global_active_session_cap_applies_across_principals() {
    let limits = LimitsConfig {
        initialize_requests_per_second: 100,
        initialize_rate_burst: 100,
        max_active_sessions_per_principal: 10,
        max_active_sessions_global: 1,
        ..LimitsConfig::default()
    };
    let fixture = fixture_router_with_permissions(&["memory:read"], limits).await;
    let _ = initialize(&fixture.app).await;
    let bob = fixture
        .app
        .oneshot(initialize_request(
            Some(&format!("Bearer {BOB_TOKEN}")),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(bob.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn session_owner_cannot_be_hijacked_by_another_principal() {
    let fixture = fixture_router().await;
    let (session_id, _) = initialize(&fixture.app).await;
    let hijacked = call_tool_with_token(
        &fixture.app,
        BOB_TOKEN,
        &session_id,
        99,
        "memory_search",
        json!({"query": "window seat"}),
    )
    .await;
    assert_eq!(hijacked.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        to_bytes(hijacked.into_body(), 1024).await.unwrap().as_ref(),
        b"forbidden"
    );
}

#[tokio::test]
async fn parallel_initialize_requests_cannot_bypass_session_caps() {
    let limits = LimitsConfig {
        initialize_requests_per_second: 100,
        initialize_rate_burst: 100,
        max_active_sessions_per_principal: 10,
        max_active_sessions_global: 1,
        ..LimitsConfig::default()
    };
    let fixture = fixture_router_with_permissions(&["memory:read"], limits).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let mut tasks = Vec::new();
    for token in [TOKEN, BOB_TOKEN] {
        let app = fixture.app.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            app.oneshot(initialize_request(Some(&format!("Bearer {token}")), None))
                .await
                .unwrap()
                .status()
        }));
    }
    barrier.wait().await;
    let mut statuses = Vec::new();
    for task in tasks {
        statuses.push(task.await.unwrap());
    }
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::TOO_MANY_REQUESTS]);
}

#[tokio::test]
async fn initialize_requests_have_a_separate_per_principal_rate_limit() {
    let limits = LimitsConfig {
        initialize_requests_per_second: 1,
        initialize_rate_burst: 1,
        max_active_sessions_per_principal: 10,
        max_active_sessions_global: 10,
        ..LimitsConfig::default()
    };
    let fixture = fixture_router_with_permissions(&["memory:read"], limits).await;
    let _ = initialize(&fixture.app).await;
    let limited = fixture
        .app
        .clone()
        .oneshot(initialize_request(Some(&format!("Bearer {TOKEN}")), None))
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.headers().get(header::RETRY_AFTER).unwrap(), "1");
    assert_eq!(
        limited.headers().get("x-ram-a-limit-reason").unwrap(),
        "initialize_rate_limit"
    );

    let bob = fixture
        .app
        .oneshot(initialize_request(
            Some(&format!("Bearer {BOB_TOKEN}")),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(bob.status(), StatusCode::OK);
}

#[tokio::test]
async fn deleting_a_session_releases_its_admission_slot() {
    let limits = LimitsConfig {
        initialize_requests_per_second: 100,
        initialize_rate_burst: 100,
        max_active_sessions_per_principal: 1,
        max_active_sessions_global: 1,
        ..LimitsConfig::default()
    };
    let fixture = fixture_router_with_permissions(&["memory:read"], limits).await;
    let (session_id, _) = initialize(&fixture.app).await;
    let deleted = fixture
        .app
        .clone()
        .oneshot(delete_session_request(TOKEN, &session_id))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::ACCEPTED);

    let _ = initialize(&fixture.app).await;
    let old_session = fixture
        .app
        .oneshot(session_request(
            &session_id,
            json!({"jsonrpc": "2.0", "id": 101, "method": "tools/list", "params": {}}),
        ))
        .await
        .unwrap();
    assert_eq!(old_session.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(start_paused = true)]
async fn idle_session_expiry_closes_the_session_and_releases_its_slot() {
    let limits = LimitsConfig {
        initialize_requests_per_second: 100,
        initialize_rate_burst: 100,
        max_active_sessions_per_principal: 1,
        max_active_sessions_global: 1,
        session_idle_timeout_seconds: 1,
        ..LimitsConfig::default()
    };
    let fixture = fixture_router_with_permissions(&["memory:read"], limits).await;
    let (expired_session_id, _) = initialize(&fixture.app).await;
    tokio::time::advance(Duration::from_secs(2)).await;

    let _ = initialize(&fixture.app).await;
    let expired = fixture
        .app
        .oneshot(session_request(
            &expired_session_id,
            json!({"jsonrpc": "2.0", "id": 102, "method": "tools/list", "params": {}}),
        ))
        .await
        .unwrap();
    assert_eq!(expired.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(start_paused = true)]
async fn authenticated_session_activity_refreshes_the_idle_deadline() {
    let limits = LimitsConfig {
        initialize_requests_per_second: 100,
        initialize_rate_burst: 100,
        max_active_sessions_per_principal: 1,
        max_active_sessions_global: 1,
        session_idle_timeout_seconds: 2,
        ..LimitsConfig::default()
    };
    let fixture = fixture_router_with_permissions(&["memory:read"], limits).await;
    let (session_id, _) = initialize(&fixture.app).await;
    tokio::time::advance(Duration::from_secs(1)).await;
    let active = fixture
        .app
        .clone()
        .oneshot(session_request(
            &session_id,
            json!({"jsonrpc": "2.0", "id": 104, "method": "tools/list", "params": {}}),
        ))
        .await
        .unwrap();
    assert_eq!(active.status(), StatusCode::OK);
    let _ = response_json(active).await;
    tokio::time::advance(Duration::from_millis(1_500)).await;

    let still_at_cap = fixture
        .app
        .oneshot(initialize_request(Some(&format!("Bearer {TOKEN}")), None))
        .await
        .unwrap();
    assert_eq!(still_at_cap.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn agent_header_must_match_and_every_mcp_response_has_a_request_id() {
    let fixture = fixture_router().await;
    let mut mismatch = initialize_request(Some(&format!("Bearer {TOKEN}")), None);
    mismatch
        .headers_mut()
        .insert("x-agent-id", "agent-b".parse().unwrap());
    let forbidden = fixture.app.clone().oneshot(mismatch).await.unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    assert!(forbidden.headers().contains_key("x-request-id"));

    let mut matching = initialize_request(Some(&format!("Bearer {TOKEN}")), None);
    matching
        .headers_mut()
        .insert("x-agent-id", "agent-a".parse().unwrap());
    let initialized = fixture.app.oneshot(matching).await.unwrap();
    if initialized.status() != StatusCode::OK {
        let status = initialized.status();
        let body = to_bytes(initialized.into_body(), 1024 * 1024)
            .await
            .unwrap();
        panic!(
            "initialize returned {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    assert!(initialized.headers().contains_key("x-request-id"));
}

#[tokio::test]
async fn initialized_session_negotiates_fixed_protocol_and_lists_all_memory_and_case_tools() {
    let fixture = fixture_router().await;
    let (session_id, initialized) = initialize(&fixture.app).await;
    assert_eq!(
        initialized["result"]["protocolVersion"],
        json!("2025-11-25")
    );

    let listed = fixture
        .app
        .oneshot(session_request(
            &session_id,
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = response_json(listed).await;
    let mut names = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        vec![
            "memory_case_delete",
            "memory_case_prepare_delete",
            "memory_case_prepare_update",
            "memory_case_prepare_upload",
            "memory_case_search",
            "memory_case_update",
            "memory_case_upload",
            "memory_ingest",
            "memory_search"
        ]
    );
}

#[tokio::test]
async fn disabled_memory_feature_hides_memory_tools_and_returns_structured_errors() {
    let fixture = fixture_router_with_features(FeatureFlags {
        memory: false,
        case_library: true,
    })
    .await;
    let (session_id, _) = initialize(&fixture.app).await;

    let listed = fixture
        .app
        .clone()
        .oneshot(session_request(
            &session_id,
            json!({"jsonrpc": "2.0", "id": 200, "method": "tools/list", "params": {}}),
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = response_json(listed).await;
    let mut names = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        vec![
            "memory_case_delete",
            "memory_case_prepare_delete",
            "memory_case_prepare_update",
            "memory_case_prepare_upload",
            "memory_case_search",
            "memory_case_update",
            "memory_case_upload"
        ]
    );

    let called = call_tool(
        &fixture.app,
        &session_id,
        201,
        "memory_search",
        json!({"query": "window seat"}),
    )
    .await;
    assert_eq!(called.status(), StatusCode::OK);
    let called = response_json(called).await;
    assert_eq!(called["result"]["isError"], json!(true));
    assert_eq!(
        called["result"]["structuredContent"]["code"],
        json!("MEMORY_DISABLED")
    );
}

#[tokio::test]
async fn disabled_case_library_feature_hides_case_tool_and_returns_structured_error() {
    let fixture = fixture_router_with_features(FeatureFlags {
        memory: true,
        case_library: false,
    })
    .await;
    let (session_id, _) = initialize(&fixture.app).await;

    let listed = fixture
        .app
        .clone()
        .oneshot(session_request(
            &session_id,
            json!({"jsonrpc": "2.0", "id": 202, "method": "tools/list", "params": {}}),
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = response_json(listed).await;
    let names = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["memory_ingest", "memory_search"]);

    let called = call_tool(
        &fixture.app,
        &session_id,
        203,
        "memory_case_search",
        json!({"query": "DNS failure"}),
    )
    .await;
    assert_eq!(called.status(), StatusCode::OK);
    let called = response_json(called).await;
    assert_eq!(called["result"]["isError"], json!(true));
    assert_eq!(
        called["result"]["structuredContent"]["code"],
        json!("CASE_LIBRARY_DISABLED")
    );
}

#[tokio::test]
async fn initialize_rejects_protocol_versions_other_than_2025_11_25() {
    let fixture = fixture_router().await;
    let response = fixture
        .app
        .oneshot(initialize_request_with_version(
            Some(&format!("Bearer {TOKEN}")),
            None,
            "2025-06-18",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn established_session_requires_the_fixed_protocol_header() {
    let fixture = fixture_router().await;
    let (session_id, _) = initialize(&fixture.app).await;
    let mut old_version = session_request(
        &session_id,
        json!({"jsonrpc": "2.0", "id": 105, "method": "tools/list", "params": {}}),
    );
    old_version
        .headers_mut()
        .insert("mcp-protocol-version", "2025-06-18".parse().unwrap());
    let old_version = fixture.app.clone().oneshot(old_version).await.unwrap();
    assert_eq!(old_version.status(), StatusCode::BAD_REQUEST);

    let mut missing = session_request(
        &session_id,
        json!({"jsonrpc": "2.0", "id": 106, "method": "tools/list", "params": {}}),
    );
    missing.headers_mut().remove("mcp-protocol-version");
    let missing = fixture.app.oneshot(missing).await.unwrap();
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn memory_search_returns_structured_content_and_json_text_fallback() {
    let fixture = fixture_router().await;
    let (session_id, _) = initialize(&fixture.app).await;
    let called = call_tool(
        &fixture.app,
        &session_id,
        3,
        "memory_search",
        json!({"query": "window seat", "top_k": 5}),
    )
    .await;
    assert_eq!(called.status(), StatusCode::OK);
    let called = response_json(called).await;
    let result = &called["result"];
    assert_eq!(result["isError"], json!(false));
    assert_eq!(result["structuredContent"], json!({"memories": []}));
    let fallback: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(fallback, result["structuredContent"]);
}

#[tokio::test]
async fn invalid_tool_input_is_a_tool_execution_error_not_a_protocol_error() {
    let fixture = fixture_router().await;
    let (session_id, _) = initialize(&fixture.app).await;
    let called = call_tool(
        &fixture.app,
        &session_id,
        4,
        "memory_search",
        json!({"query": "", "top_k": 5}),
    )
    .await;
    assert_eq!(called.status(), StatusCode::OK);
    let called = response_json(called).await;
    assert!(called.get("error").is_none(), "{called}");
    assert_eq!(called["result"]["isError"], json!(true));
    assert_eq!(
        called["result"]["structuredContent"]["code"],
        json!("INVALID_REQUEST")
    );
    assert_eq!(
        called["result"]["structuredContent"]["retriable"],
        json!(false)
    );
}

#[tokio::test]
async fn memory_ingest_runs_the_service_and_returns_structured_output() {
    let fixture = fixture_router().await;
    let (session_id, _) = initialize(&fixture.app).await;
    let called = call_tool(
        &fixture.app,
        &session_id,
        5,
        "memory_ingest",
        json!({
            "conversation_id": "conversation-1",
            "messages": [{
                "id": "message-1",
                "role": "user",
                "speaker": "Alice",
                "text": "I prefer a window seat.",
                "timestamp": "2026-07-22T10:00:00Z",
                "candidate": true
            }]
        }),
    )
    .await;
    assert_eq!(called.status(), StatusCode::OK);
    let called = response_json(called).await;
    let result = &called["result"];
    assert_eq!(result["isError"], json!(false));
    assert_eq!(result["structuredContent"]["accepted_count"], json!(1));
    assert_eq!(result["structuredContent"]["idempotency_hit"], json!(false));
    let fallback: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(fallback, result["structuredContent"]);
}

#[tokio::test]
async fn missing_tool_permission_is_rejected_with_http_forbidden() {
    let fixture = fixture_router_with_permissions(&["memory:read"], LimitsConfig::default()).await;
    let (session_id, _) = initialize(&fixture.app).await;
    let denied = call_tool(
        &fixture.app,
        &session_id,
        6,
        "memory_ingest",
        json!({
            "conversation_id": "conversation-1",
            "messages": [{"id": "message-1", "role": "user", "text": "hello"}]
        }),
    )
    .await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert!(denied.headers().contains_key("x-request-id"));
    let body = to_bytes(denied.into_body(), 1024).await.unwrap();
    assert_eq!(body.as_ref(), b"forbidden");
}

#[tokio::test]
async fn missing_case_search_permission_is_rejected_with_http_forbidden() {
    let fixture = fixture_router_with_permissions(&["memory:read"], LimitsConfig::default()).await;
    let (session_id, _) = initialize(&fixture.app).await;
    let denied = call_tool(
        &fixture.app,
        &session_id,
        107,
        "memory_case_search",
        json!({"query": "WiFi DNS failure"}),
    )
    .await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert!(denied.headers().contains_key("x-request-id"));
    let body = to_bytes(denied.into_body(), 1024).await.unwrap();
    assert_eq!(body.as_ref(), b"forbidden");
}

#[tokio::test]
async fn case_document_mutation_tools_require_cases_write_permission() {
    let read_only =
        fixture_router_with_permissions(&["cases:read"], LimitsConfig::default()).await;
    let (read_only_session, _) = initialize(&read_only.app).await;
    let denied = call_tool(
        &read_only.app,
        &read_only_session,
        108,
        "memory_case_prepare_delete",
        json!({
            "document_id": "dns-case",
            "deletion_reason": "The case is obsolete."
        }),
    )
    .await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let body = to_bytes(denied.into_body(), 1024).await.unwrap();
    assert_eq!(body.as_ref(), b"forbidden");

    let writer = fixture_router_with_permissions(&["cases:write"], LimitsConfig::default()).await;
    let (writer_session, _) = initialize(&writer.app).await;
    let accepted_by_transport = call_tool(
        &writer.app,
        &writer_session,
        109,
        "memory_case_prepare_update",
        json!({
            "document_id": "dns-case",
            "file_name": "dns-case.md",
            "diagnosis_summary": "The previous recovery procedure is incomplete.",
            "content": "# Updated DNS procedure"
        }),
    )
    .await;
    assert_eq!(accepted_by_transport.status(), StatusCode::OK);
    let accepted_by_transport = response_json(accepted_by_transport).await;
    assert_eq!(accepted_by_transport["result"]["isError"], json!(true));
    assert_eq!(
        accepted_by_transport["result"]["structuredContent"]["code"],
        json!("CASE_NOT_CONFIGURED")
    );
}

#[tokio::test]
async fn mcp_upload_update_and_delete_flow_reaches_the_embedded_case_library() {
    let case_temp = TempDir::new().unwrap();
    let case_service = build_service(&CaseServiceOptions {
        rag_store: case_temp.path().join("cases.sqlite"),
        memory_store: case_temp.path().join("case-index.sqlite"),
        embedding_provider: memory_cases::EmbeddingProviderKind::Hash,
        embedding_api_key_env: "UNUSED_CASE_EMBEDDING_KEY".to_owned(),
        embedding_base_url: "http://127.0.0.1:1/v1".to_owned(),
        embedding_model: "hash".to_owned(),
        embedding_dimensions: 32,
        chunk_size: 64,
        summary_llm_model: None,
        summary_llm_api_key_env: "UNUSED_CASE_SUMMARY_KEY".to_owned(),
        summary_llm_base_url: "http://127.0.0.1:1/v1".to_owned(),
        summary_llm_timeout_ms: 1_000,
    })
    .unwrap();
    let case_provider: DynCaseSearchProvider = Arc::new(EmbeddedCaseSearchProvider::new(
        case_service.clone(),
        "ops".to_owned(),
        &[CaseLibraryConfig {
            name: "ops".to_owned(),
            dataset_id: "ops-cases".to_owned(),
            tenant_ids: vec!["tenant-a".to_owned()],
        }],
    ));
    let fixture = fixture_router_with_schema_and_features(
        &["cases:read", "cases:write"],
        LimitsConfig::default(),
        Duration::ZERO,
        true,
        true,
        FeatureFlags::all(),
        Some(case_provider),
    )
    .await;
    let (session_id, _) = initialize(&fixture.app).await;

    let upload_proposal = call_tool(
        &fixture.app,
        &session_id,
        110,
        "memory_case_prepare_upload",
        json!({
            "library": "ops",
            "document_id": "dns-case",
            "file_name": "dns-case.md",
            "diagnosis_summary": "The DNS resolver returned a stale cached record.",
            "content": "# DNS failure\n\nFlush mcpolddnsneedle from the resolver cache."
        }),
    )
    .await;
    assert_eq!(upload_proposal.status(), StatusCode::OK);
    let upload_proposal = response_json(upload_proposal).await;
    assert_eq!(upload_proposal["result"]["isError"], json!(false));
    assert_eq!(
        upload_proposal["result"]["structuredContent"]["operation"],
        json!("upload")
    );
    assert_eq!(
        upload_proposal["result"]["structuredContent"]["document_id"],
        json!("dns-case")
    );
    assert!(case_service.list_datasets().unwrap().datasets.is_empty());
    let upload_token = upload_proposal["result"]["structuredContent"]["confirmation_token"]
        .as_str()
        .unwrap()
        .to_owned();

    let unconfirmed = call_tool(
        &fixture.app,
        &session_id,
        111,
        "memory_case_upload",
        json!({"confirmation_token": upload_token, "user_confirmed": false}),
    )
    .await;
    assert_eq!(unconfirmed.status(), StatusCode::OK);
    let unconfirmed = response_json(unconfirmed).await;
    assert_eq!(
        unconfirmed["result"]["structuredContent"]["code"],
        json!("CASE_USER_CONFIRMATION_REQUIRED")
    );
    assert!(case_service.list_datasets().unwrap().datasets.is_empty());

    let uploaded = call_tool(
        &fixture.app,
        &session_id,
        112,
        "memory_case_upload",
        json!({"confirmation_token": upload_token, "user_confirmed": true}),
    )
    .await;
    assert_eq!(uploaded.status(), StatusCode::OK);
    let uploaded = response_json(uploaded).await;
    assert_eq!(uploaded["result"]["isError"], json!(false));
    assert_eq!(
        uploaded["result"]["structuredContent"]["ingestion_status"],
        json!("pending")
    );
    assert!(case_service.run_next_ingestion_task().await.unwrap());

    let old_search = call_tool(
        &fixture.app,
        &session_id,
        113,
        "memory_case_search",
        json!({"query": "mcpolddnsneedle", "library": "ops", "top_k": 5}),
    )
    .await;
    let old_search = response_json(old_search).await;
    assert_eq!(
        old_search["result"]["structuredContent"]["references"][0]["document_id"],
        json!("dns-case")
    );

    let update_proposal = call_tool(
        &fixture.app,
        &session_id,
        114,
        "memory_case_prepare_update",
        json!({
            "library": "ops",
            "document_id": "dns-case",
            "file_name": "dns-case.md",
            "diagnosis_summary": "The initial mitigation did not restart the resolver.",
            "content": "# Updated DNS recovery\n\nRestart mcpnewdnsneedle after validation."
        }),
    )
    .await;
    assert_eq!(update_proposal.status(), StatusCode::OK);
    let update_proposal = response_json(update_proposal).await;
    assert_eq!(update_proposal["result"]["isError"], json!(false));
    assert_eq!(
        update_proposal["result"]["structuredContent"]["operation"],
        json!("update")
    );
    let update_token = update_proposal["result"]["structuredContent"]["confirmation_token"]
        .as_str()
        .unwrap()
        .to_owned();

    let updated = call_tool(
        &fixture.app,
        &session_id,
        115,
        "memory_case_update",
        json!({"confirmation_token": update_token, "user_confirmed": true}),
    )
    .await;
    assert_eq!(updated.status(), StatusCode::OK);
    let updated = response_json(updated).await;
    assert_eq!(updated["result"]["isError"], json!(false));
    assert!(case_service.run_next_ingestion_task().await.unwrap());

    let new_search = call_tool(
        &fixture.app,
        &session_id,
        116,
        "memory_case_search",
        json!({"query": "mcpnewdnsneedle", "library": "ops", "top_k": 5}),
    )
    .await;
    let new_search = response_json(new_search).await;
    assert!(new_search["result"]["structuredContent"]["references"][0]["content"]
        .as_str()
        .unwrap()
        .contains("mcpnewdnsneedle"));

    let delete_proposal = call_tool(
        &fixture.app,
        &session_id,
        117,
        "memory_case_prepare_delete",
        json!({
            "library": "ops",
            "document_id": "dns-case",
            "deletion_reason": "The recovery procedure is obsolete and should not be suggested."
        }),
    )
    .await;
    assert_eq!(delete_proposal.status(), StatusCode::OK);
    let delete_proposal = response_json(delete_proposal).await;
    assert_eq!(delete_proposal["result"]["isError"], json!(false));
    assert_eq!(
        delete_proposal["result"]["structuredContent"]["operation"],
        json!("delete")
    );
    assert_eq!(
        delete_proposal["result"]["structuredContent"]["document_id"],
        json!("dns-case")
    );
    let delete_token = delete_proposal["result"]["structuredContent"]["confirmation_token"]
        .as_str()
        .unwrap()
        .to_owned();

    let unconfirmed_delete = call_tool(
        &fixture.app,
        &session_id,
        118,
        "memory_case_delete",
        json!({"confirmation_token": delete_token, "user_confirmed": false}),
    )
    .await;
    assert_eq!(unconfirmed_delete.status(), StatusCode::OK);
    let unconfirmed_delete = response_json(unconfirmed_delete).await;
    assert_eq!(
        unconfirmed_delete["result"]["structuredContent"]["code"],
        json!("CASE_USER_CONFIRMATION_REQUIRED")
    );
    assert_eq!(
        case_service
            .list_documents("ops-cases")
            .unwrap()
            .documents
            .len(),
        1
    );

    let deleted = call_tool(
        &fixture.app,
        &session_id,
        119,
        "memory_case_delete",
        json!({"confirmation_token": delete_token, "user_confirmed": true}),
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::OK);
    let deleted = response_json(deleted).await;
    assert_eq!(deleted["result"]["isError"], json!(false));
    assert_eq!(
        deleted["result"]["structuredContent"]["deleted"],
        json!(true)
    );
    assert!(case_service
        .list_documents("ops-cases")
        .unwrap()
        .documents
        .is_empty());

    let deleted_search = call_tool(
        &fixture.app,
        &session_id,
        120,
        "memory_case_search",
        json!({"query": "mcpnewdnsneedle", "library": "ops", "top_k": 5}),
    )
    .await;
    let deleted_search = response_json(deleted_search).await;
    assert_eq!(
        deleted_search["result"]["structuredContent"]["references"],
        json!([])
    );
}

#[tokio::test]
async fn mcp_body_limit_returns_payload_too_large() {
    let limits = LimitsConfig {
        max_body_bytes: 256,
        ..LimitsConfig::default()
    };
    let fixture = fixture_router_with_permissions(&["memory:read", "memory:write"], limits).await;
    let oversized = mcp_request(
        Some(&format!("Bearer {TOKEN}")),
        None,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "x".repeat(512), "version": "1"}
            }
        }),
    );
    let response = fixture.app.oneshot(oversized).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(body.as_ref(), b"payload too large");
}

#[tokio::test]
async fn tool_rate_limit_is_scoped_to_the_authenticated_principal_and_tool() {
    let limits = LimitsConfig {
        requests_per_second: 1,
        rate_burst: 1,
        ..LimitsConfig::default()
    };
    let fixture =
        fixture_router_with_permissions(&["memory:read", "memory:write", "cases:read"], limits)
            .await;
    let (alice_session, _) = initialize(&fixture.app).await;
    let (bob_session, _) = initialize_with_token(&fixture.app, BOB_TOKEN).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(6));
    let (_, alice_search_a, alice_search_b, bob_search, other_tool, case_search) = tokio::join!(
        barrier.wait(),
        async {
            barrier.wait().await;
            call_tool(
                &fixture.app,
                &alice_session,
                7,
                "memory_search",
                json!({"query": "window seat"}),
            )
            .await
        },
        async {
            barrier.wait().await;
            call_tool(
                &fixture.app,
                &alice_session,
                8,
                "memory_search",
                json!({"query": "window seat"}),
            )
            .await
        },
        async {
            barrier.wait().await;
            call_tool_with_token(
                &fixture.app,
                BOB_TOKEN,
                &bob_session,
                9,
                "memory_search",
                json!({"query": "window seat"}),
            )
            .await
        },
        async {
            barrier.wait().await;
            call_tool(
                &fixture.app,
                &alice_session,
                10,
                "memory_ingest",
                json!({
                    "conversation_id": "rate-scope",
                    "messages": [{"id": "rate-message", "role": "user", "text": "hello"}]
                }),
            )
            .await
        },
        async {
            barrier.wait().await;
            call_tool(
                &fixture.app,
                &alice_session,
                108,
                "memory_case_search",
                json!({"query": "WiFi DNS failure"}),
            )
            .await
        }
    );
    assert_eq!(bob_search.status(), StatusCode::OK);
    assert_eq!(other_tool.status(), StatusCode::OK);
    assert_eq!(case_search.status(), StatusCode::OK);
    let (allowed, limited) = if alice_search_a.status() == StatusCode::OK {
        (alice_search_a, alice_search_b)
    } else {
        (alice_search_b, alice_search_a)
    };
    assert_eq!(allowed.status(), StatusCode::OK);
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.headers().get(header::RETRY_AFTER).unwrap(), "1");
    assert_eq!(
        limited.headers().get("x-ram-a-limit-reason").unwrap(),
        "tool_rate_limit"
    );
    let body = to_bytes(limited.into_body(), 1024).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert_eq!(body, "too many requests");
    for secret in [
        TOKEN,
        BOB_TOKEN,
        "alice",
        "tenant-a",
        "memory.sqlite",
        "embedding-model",
    ] {
        assert!(!body.contains(secret));
    }
}

#[tokio::test]
async fn concurrent_tool_limit_rejects_excess_work_without_queueing() {
    let limits = LimitsConfig {
        requests_per_second: 100,
        rate_burst: 100,
        max_in_flight_per_principal_tool: 1,
        ..LimitsConfig::default()
    };
    let fixture = fixture_router_with_options(
        &["memory:read", "memory:write"],
        limits,
        Duration::from_millis(250),
    )
    .await;
    let (session_id, _) = initialize(&fixture.app).await;
    let app = fixture.app.clone();
    let first_session = session_id.clone();
    let first = tokio::spawn(async move {
        call_tool(
            &app,
            &first_session,
            9,
            "memory_ingest",
            json!({
                "conversation_id": "conversation-1",
                "messages": [{"id": "message-1", "role": "user", "text": "hello"}]
            }),
        )
        .await
    });
    fixture.extract_started.notified().await;

    let second = call_tool(
        &fixture.app,
        &session_id,
        10,
        "memory_ingest",
        json!({
            "conversation_id": "conversation-2",
            "messages": [{"id": "message-2", "role": "user", "text": "hello again"}]
        }),
    )
    .await;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        second.headers().get("x-ram-a-limit-reason").unwrap(),
        "tool_concurrency"
    );
    assert_eq!(first.await.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn cancellation_ends_an_active_sse_response_without_waiting_for_the_tool() {
    let fixture = fixture_router_with_options(
        &["memory:read", "memory:write"],
        LimitsConfig::default(),
        Duration::from_secs(60),
    )
    .await;
    let (session_id, _) = initialize(&fixture.app).await;
    let response = call_tool(
        &fixture.app,
        &session_id,
        103,
        "memory_ingest",
        json!({
            "conversation_id": "shutdown",
            "messages": [{"id": "shutdown-message", "role": "user", "text": "hello"}]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    fixture.extract_started.notified().await;

    let tool_future_dropped = fixture.extract_dropped.notified();
    fixture.cancellation_token.cancel();
    tokio::time::timeout(
        Duration::from_secs(1),
        to_bytes(response.into_body(), 1024 * 1024),
    )
    .await
    .expect("cancelled SSE response should end promptly")
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), tool_future_dropped)
        .await
        .expect("cancellation should drop the active tool future");
}

#[tokio::test]
async fn health_is_liveness_while_readiness_checks_dependencies_and_capacity() {
    let not_ready = fixture_router_full(
        &["memory:read", "memory:write"],
        LimitsConfig::default(),
        Duration::ZERO,
        false,
    )
    .await;
    let health = not_ready
        .app
        .clone()
        .oneshot(Request::get("/healthy").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    let readiness = not_ready
        .app
        .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(readiness.status(), StatusCode::SERVICE_UNAVAILABLE);

    let ready = fixture_router().await;
    assert!(ready.database_path.exists());
    let readiness = ready
        .app
        .clone()
        .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(readiness.status(), StatusCode::OK);
    std::fs::remove_file(&ready.database_path).unwrap();
    let readiness = ready
        .app
        .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(readiness.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!ready.database_path.exists());

    let no_capacity = fixture_router_full(
        &["memory:read", "memory:write"],
        LimitsConfig {
            max_in_flight_per_principal_tool: 0,
            ..LimitsConfig::default()
        },
        Duration::ZERO,
        true,
    )
    .await;
    let readiness = no_capacity
        .app
        .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(readiness.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn readiness_requires_both_memory_and_idempotency_schemas() {
    let fixture = fixture_router_with_schema(
        &["memory:read", "memory:write"],
        LimitsConfig::default(),
        Duration::ZERO,
        true,
        false,
    )
    .await;
    assert!(fixture.database_path.exists());
    let readiness = fixture
        .app
        .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(readiness.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[test]
fn external_bind_requires_explicit_tls_termination_acknowledgement() {
    let local = HttpConfig::default();
    assert!(local.bind_address.is_loopback());
    assert!(local.validate_bind().is_ok());

    let mut external = HttpConfig {
        bind_address: "0.0.0.0".parse().unwrap(),
        ..HttpConfig::default()
    };
    assert!(external.validate_bind().is_err());
    external.tls_termination_acknowledged = true;
    assert!(external.validate_bind().is_err());
    external.allowed_hosts = vec!["memory.example.test".to_string()];
    assert!(external.validate_bind().is_ok());
}

#[test]
fn production_runtime_config_requires_live_components_and_nonzero_limits() {
    let auth = AuthConfig {
        tokens: vec![TokenConfig {
            token_env: "RAM_A_SERVER_TOKEN".to_string(),
            tenant_id: "tenant-a".to_string(),
            user_id: "alice".to_string(),
            agent_id: "agent-a".to_string(),
            permissions: vec!["memory:read".to_string(), "memory:write".to_string()],
        }],
    };
    let incomplete = ServerConfig {
        auth: auth.clone(),
        features: Default::default(),
        http: HttpConfig::default(),
        limits: LimitsConfig::default(),
        storage: None,
        providers: None,
        case_library: None,
        graph_memory: None,
    };
    assert!(incomplete.validate_runtime().is_err());

    let mut complete = ServerConfig {
        auth,
        features: Default::default(),
        http: HttpConfig::default(),
        limits: LimitsConfig::default(),
        storage: Some(StorageConfig {
            database_path: "memory.sqlite".into(),
        }),
        providers: Some(ProvidersConfig {
            api_key_env: "RAM_A_PROVIDER_KEY".to_string(),
            base_url: "https://provider.example/v1".to_string(),
            embedding_provider: EmbeddingProviderKind::OpenAiCompatible,
            embedding_api_key_env: None,
            embedding_base_url: None,
            embedding_model: "embedding-model".to_string(),
            embedding_dimensions: 32,
            extractor_model: "extractor-model".to_string(),
            verifier_model: "verifier-model".to_string(),
            timeout_seconds: 30,
            max_retries: 2,
        }),
        case_library: None,
        graph_memory: None,
    };
    assert!(complete.validate_runtime().is_ok());
    for invalid_base_url in [
        "provider.example/v1",
        "ftp://provider.example/v1",
        "https:///v1",
        "not a url",
    ] {
        let mut invalid_url = complete.clone();
        invalid_url.providers.as_mut().unwrap().base_url = invalid_base_url.to_string();
        assert!(
            invalid_url.validate_runtime().is_err(),
            "accepted {invalid_base_url}"
        );
    }
    let mut no_principals = complete.clone();
    no_principals.auth.tokens.clear();
    assert!(no_principals.validate_runtime().is_err());
    complete.limits.max_in_flight_per_principal_tool = 0;
    assert!(complete.validate_runtime().is_err());
}

#[test]
fn production_runtime_config_resolves_memory_and_case_library_feature_switches() {
    let no_case_library: ServerConfig = serde_json::from_str(
        r#"{
          "auth": {
            "tokens": [{
              "token_env": "RAM_A_SERVER_TOKEN",
              "tenant_id": "tenant-a",
              "user_id": "alice",
              "agent_id": "agent-a",
              "permissions": ["memory:read", "memory:write"]
            }]
          },
          "storage": {
            "database_path": "memory.sqlite"
          },
          "providers": {
            "api_key_env": "RAM_A_PROVIDER_KEY",
            "base_url": "http://127.0.0.1:8088/v1",
            "embedding_provider": "hash",
            "embedding_model": "hash",
            "embedding_dimensions": 1024,
            "extractor_model": "GLM-5.2",
            "verifier_model": "GLM-5.2"
          }
        }"#,
    )
    .unwrap();
    assert_eq!(
        no_case_library
            .features
            .resolve(no_case_library.case_library.is_some()),
        FeatureFlags {
            memory: true,
            case_library: false
        }
    );
    assert!(no_case_library.validate_runtime().is_ok());

    let explicit_disabled: ServerConfig = serde_json::from_str(
        r#"{
          "auth": {
            "tokens": [{
              "token_env": "RAM_A_SERVER_TOKEN",
              "tenant_id": "tenant-a",
              "user_id": "alice",
              "agent_id": "agent-a",
              "permissions": ["memory:read", "memory:write", "cases:read"]
            }]
          },
          "features": {
            "memory": {"enabled": false},
            "case_library": {"enabled": false}
          },
          "storage": {
            "database_path": "memory.sqlite"
          },
          "providers": {
            "api_key_env": "RAM_A_PROVIDER_KEY",
            "base_url": "http://127.0.0.1:8088/v1",
            "embedding_provider": "hash",
            "embedding_model": "hash",
            "embedding_dimensions": 1024,
            "extractor_model": "GLM-5.2",
            "verifier_model": "GLM-5.2"
          }
        }"#,
    )
    .unwrap();
    assert_eq!(
        explicit_disabled
            .features
            .resolve(explicit_disabled.case_library.is_some()),
        FeatureFlags {
            memory: false,
            case_library: false
        }
    );
    assert!(explicit_disabled.validate_runtime().is_ok());

    let invalid_case_enabled_without_library: ServerConfig = serde_json::from_str(
        r#"{
          "auth": {
            "tokens": [{
              "token_env": "RAM_A_SERVER_TOKEN",
              "tenant_id": "tenant-a",
              "user_id": "alice",
              "agent_id": "agent-a",
              "permissions": ["memory:read", "memory:write", "cases:read"]
            }]
          },
          "features": {
            "case_library": {"enabled": true}
          },
          "storage": {
            "database_path": "memory.sqlite"
          },
          "providers": {
            "api_key_env": "RAM_A_PROVIDER_KEY",
            "base_url": "http://127.0.0.1:8088/v1",
            "embedding_provider": "hash",
            "embedding_model": "hash",
            "embedding_dimensions": 1024,
            "extractor_model": "GLM-5.2",
            "verifier_model": "GLM-5.2"
          }
        }"#,
    )
    .unwrap();
    assert!(invalid_case_enabled_without_library
        .validate_runtime()
        .is_err());

    let enabled_case_library: ServerConfig = serde_json::from_str(
        r#"{
          "auth": {
            "tokens": [{
              "token_env": "RAM_A_SERVER_TOKEN",
              "tenant_id": "tenant-a",
              "user_id": "alice",
              "agent_id": "agent-a",
              "permissions": ["memory:read", "memory:write", "cases:read"]
            }]
          },
          "storage": {
            "database_path": "memory.sqlite"
          },
          "providers": {
            "api_key_env": "RAM_A_PROVIDER_KEY",
            "base_url": "http://127.0.0.1:8088/v1",
            "embedding_provider": "hash",
            "embedding_model": "hash",
            "embedding_dimensions": 1024,
            "extractor_model": "GLM-5.2",
            "verifier_model": "GLM-5.2"
          },
          "case_library": {
            "rag_store": "cases.sqlite",
            "index_store": "cases-index.sqlite",
            "api_token_env": "RAM_A_CASES_ADMIN_TOKEN",
            "ingestion_poll_ms": 250,
            "embedding_provider": "hash",
            "embedding_model": "hash",
            "embedding_dimensions": 1024,
            "default_library": "ops",
            "libraries": [{
              "name": "ops",
              "dataset_id": "openeuler-ops-cases",
              "tenant_ids": ["tenant-a"]
            }]
          }
        }"#,
    )
    .unwrap();
    assert_eq!(
        enabled_case_library
            .features
            .resolve(enabled_case_library.case_library.is_some()),
        FeatureFlags {
            memory: true,
            case_library: true
        }
    );
    let case_library = enabled_case_library.case_library.as_ref().unwrap();
    assert_eq!(
        case_library.api_token_env.as_deref(),
        Some("RAM_A_CASES_ADMIN_TOKEN")
    );
    assert_eq!(case_library.ingestion_poll_ms, 250);
    assert!(enabled_case_library.validate_runtime().is_ok());
}

#[test]
fn production_runtime_config_keeps_graph_memory_disabled_by_default() {
    let config: ServerConfig = serde_json::from_str(
        r#"{
          "auth": {
            "tokens": [{
              "token_env": "RAM_A_SERVER_TOKEN",
              "tenant_id": "tenant-a",
              "user_id": "alice",
              "agent_id": "agent-a",
              "permissions": ["memory:read", "memory:write"]
            }]
          },
          "storage": {"database_path": "memory.sqlite"},
          "providers": {
            "api_key_env": "RAM_A_PROVIDER_KEY",
            "base_url": "http://127.0.0.1:8088/v1",
            "embedding_provider": "hash",
            "embedding_model": "hash",
            "embedding_dimensions": 1024,
            "extractor_model": "GLM-5.2",
            "verifier_model": "GLM-5.2"
          }
        }"#,
    )
    .unwrap();

    assert!(!config.features.graph_memory.enabled);
    assert!(config.graph_memory.is_none());
    assert!(config.validate_runtime().is_ok());
}

#[test]
fn production_runtime_config_requires_graph_configuration_when_enabled() {
    let config: ServerConfig = serde_json::from_str(
        r#"{
          "auth": {
            "tokens": [{
              "token_env": "RAM_A_SERVER_TOKEN",
              "tenant_id": "tenant-a",
              "user_id": "alice",
              "agent_id": "agent-a",
              "permissions": ["memory:read", "memory:write"]
            }]
          },
          "features": {"graph_memory": {"enabled": true}},
          "storage": {"database_path": "memory.sqlite"},
          "providers": {
            "api_key_env": "RAM_A_PROVIDER_KEY",
            "base_url": "http://127.0.0.1:8088/v1",
            "embedding_provider": "hash",
            "embedding_model": "hash",
            "embedding_dimensions": 1024,
            "extractor_model": "GLM-5.2",
            "verifier_model": "GLM-5.2"
          }
        }"#,
    )
    .unwrap();

    let error = config.validate_runtime().unwrap_err().to_string();
    assert!(error.contains("graph_memory feature requires graph_memory configuration"));
}

#[test]
fn production_runtime_config_rejects_graph_memory_without_memory_tools() {
    let config: ServerConfig = serde_json::from_str(
        r#"{
          "auth": {
            "tokens": [{
              "token_env": "RAM_A_SERVER_TOKEN",
              "tenant_id": "tenant-a",
              "user_id": "alice",
              "agent_id": "agent-a",
              "permissions": ["memory:read", "memory:write"]
            }]
          },
          "features": {
            "memory": {"enabled": false},
            "graph_memory": {"enabled": true}
          },
          "storage": {"database_path": "memory.sqlite"},
          "providers": {
            "api_key_env": "RAM_A_PROVIDER_KEY",
            "base_url": "http://127.0.0.1:8088/v1",
            "embedding_provider": "hash",
            "embedding_model": "hash",
            "embedding_dimensions": 1024,
            "extractor_model": "GLM-5.2",
            "verifier_model": "GLM-5.2"
          },
          "graph_memory": {
            "llm_api_key_env": "RAM_A_GRAPH_KEY",
            "llm_model": "graph-model"
          }
        }"#,
    )
    .unwrap();

    let error = config.validate_runtime().unwrap_err().to_string();
    assert!(error.contains("graph_memory feature requires the memory feature"));
}

#[test]
fn production_runtime_config_maps_valid_graph_retrieval_settings() {
    let mut config: ServerConfig = serde_json::from_str(
        r#"{
          "auth": {
            "tokens": [{
              "token_env": "RAM_A_SERVER_TOKEN",
              "tenant_id": "tenant-a",
              "user_id": "alice",
              "agent_id": "agent-a",
              "permissions": ["memory:read", "memory:write"]
            }]
          },
          "features": {"graph_memory": {"enabled": true}},
          "storage": {"database_path": "memory.sqlite"},
          "providers": {
            "api_key_env": "RAM_A_PROVIDER_KEY",
            "base_url": "http://127.0.0.1:8088/v1",
            "embedding_provider": "hash",
            "embedding_model": "hash",
            "embedding_dimensions": 1024,
            "extractor_model": "GLM-5.2",
            "verifier_model": "GLM-5.2"
          },
          "graph_memory": {
            "llm_api_key_env": "RAM_A_GRAPH_KEY",
            "llm_base_url": "http://127.0.0.1:8089/v1",
            "llm_model": "graph-model",
            "llm_timeout_ms": 45000,
            "build_concurrency": 3,
            "retrieval": {
              "weight": 0.35,
              "rerank_with_graph": true,
              "allow_graph_only": true,
              "max_graph_only_results": 4,
              "seed_limit": 60,
              "max_evidence_records_per_fact": 2,
              "fail_open": true
            }
          }
        }"#,
    )
    .unwrap();

    assert!(config.validate_runtime().is_ok());
    let graph = config.graph_memory.as_ref().unwrap();
    assert_eq!(graph.build_concurrency, 3);
    assert_eq!(
        graph.retrieval,
        GraphMemoryRetrievalConfig {
            weight: 0.35,
            rerank_with_graph: true,
            allow_graph_only: true,
            max_graph_only_results: Some(4),
            seed_limit: Some(60),
            max_evidence_records_per_fact: Some(2),
            fail_open: true,
        }
    );
    let core = graph.retrieval.core_config();
    assert!(core.enabled);
    assert_eq!(core.weight, 0.35);
    assert!(core.rerank_with_graph);
    assert!(core.allow_graph_only);

    config.graph_memory.as_mut().unwrap().retrieval.seed_limit =
        Some(memory_core::MAX_GRAPH_SEED_LIMIT + 1);
    assert!(config.validate_runtime().is_err());
}

#[test]
fn production_runtime_config_accepts_hash_embedding_provider() {
    let config: ServerConfig = serde_json::from_str(
        r#"{
          "auth": {
            "tokens": [{
              "token_env": "RAM_A_SERVER_TOKEN",
              "tenant_id": "tenant-a",
              "user_id": "alice",
              "agent_id": "agent-a",
              "permissions": ["memory:read", "memory:write"]
            }]
          },
          "storage": {
            "database_path": "memory.sqlite"
          },
          "providers": {
            "api_key_env": "RAM_A_PROVIDER_KEY",
            "base_url": "http://127.0.0.1:8088/v1",
            "embedding_provider": "hash",
            "embedding_model": "hash",
            "embedding_dimensions": 1024,
            "extractor_model": "GLM-5.2",
            "verifier_model": "GLM-5.2"
          }
        }"#,
    )
    .unwrap();

    assert_eq!(
        config.providers.as_ref().unwrap().embedding_provider,
        EmbeddingProviderKind::Hash
    );
    assert!(config.validate_runtime().is_ok());
}

#[test]
fn production_runtime_config_accepts_separate_embedding_endpoint() {
    let config: ServerConfig = serde_json::from_str(
        r#"{
          "auth": {
            "tokens": [{
              "token_env": "RAM_A_SERVER_TOKEN",
              "tenant_id": "tenant-a",
              "user_id": "alice",
              "agent_id": "agent-a",
              "permissions": ["memory:read", "memory:write"]
            }]
          },
          "storage": {
            "database_path": "memory.sqlite"
          },
          "providers": {
            "api_key_env": "RAM_A_CHAT_KEY",
            "base_url": "http://127.0.0.1:8088/v1",
            "embedding_provider": "openai_compatible",
            "embedding_api_key_env": "RAM_A_EMBEDDING_KEY",
            "embedding_base_url": "http://127.0.0.1:9090/v1",
            "embedding_model": "local-embedding",
            "embedding_dimensions": 1024,
            "extractor_model": "GLM-5.2",
            "verifier_model": "GLM-5.2"
          }
        }"#,
    )
    .unwrap();

    let providers = config.providers.as_ref().unwrap();
    assert_eq!(
        providers.embedding_api_key_env.as_deref(),
        Some("RAM_A_EMBEDDING_KEY")
    );
    assert_eq!(
        providers.embedding_base_url.as_deref(),
        Some("http://127.0.0.1:9090/v1")
    );
    assert!(config.validate_runtime().is_ok());
}

#[test]
fn production_runtime_config_accepts_open_router_embedding_provider_alias() {
    let config: ServerConfig = serde_json::from_str(
        r#"{
          "auth": {
            "tokens": [{
              "token_env": "RAM_A_SERVER_TOKEN",
              "tenant_id": "tenant-a",
              "user_id": "alice",
              "agent_id": "agent-a",
              "permissions": ["memory:read", "memory:write"]
            }]
          },
          "storage": {
            "database_path": "memory.sqlite"
          },
          "providers": {
            "api_key_env": "RAM_A_PROVIDER_KEY",
            "base_url": "http://127.0.0.1:8088/v1",
            "embedding_provider": "open_router",
            "embedding_model": "local-embedding",
            "embedding_dimensions": 1024,
            "extractor_model": "GLM-5.2",
            "verifier_model": "GLM-5.2"
          }
        }"#,
    )
    .unwrap();

    assert_eq!(
        config.providers.as_ref().unwrap().embedding_provider,
        EmbeddingProviderKind::OpenAiCompatible
    );
    assert!(config.validate_runtime().is_ok());
}

#[test]
fn server_binary_supports_a_default_config_path() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ram-a-mem"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Usage: ram-a-mem [OPTIONS]"), "{stdout}");
    assert!(stdout.contains("--config <CONFIG>"), "{stdout}");
}

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use axum::response::Response;
use axum::Router;
use memory_core::{HashEmbedding, MemoryManager, SqliteMemoryStore};
use memory_mcp::{
    create_http_router, AuthConfig, HttpConfig, HttpRuntime, IdempotencyRepository, LimitsConfig,
    MemoryService, ProvidersConfig, ServerConfig, StorageConfig, TokenAuthenticator, TokenConfig,
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
    let runtime = HttpRuntime::with_cancellation_token(
        service,
        Arc::new(authenticator),
        database_path.clone(),
        providers_ready,
        cancellation_token.clone(),
    );
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
async fn initialized_session_negotiates_fixed_protocol_and_lists_exactly_three_tools() {
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
        vec!["memory_case_search", "memory_ingest", "memory_search"]
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
        http: HttpConfig::default(),
        limits: LimitsConfig::default(),
        storage: None,
        providers: None,
        case_service: None,
    };
    assert!(incomplete.validate_runtime().is_err());

    let mut complete = ServerConfig {
        auth,
        http: HttpConfig::default(),
        limits: LimitsConfig::default(),
        storage: Some(StorageConfig {
            database_path: "memory.sqlite".into(),
        }),
        providers: Some(ProvidersConfig {
            api_key_env: "RAM_A_PROVIDER_KEY".to_string(),
            base_url: "https://provider.example/v1".to_string(),
            embedding_model: "embedding-model".to_string(),
            embedding_dimensions: 32,
            extractor_model: "extractor-model".to_string(),
            verifier_model: "verifier-model".to_string(),
            timeout_seconds: 30,
            max_retries: 2,
        }),
        case_service: None,
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
fn server_binary_requires_a_config_path() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ram-a-mcp-server"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--config <CONFIG>"), "{stdout}");
}

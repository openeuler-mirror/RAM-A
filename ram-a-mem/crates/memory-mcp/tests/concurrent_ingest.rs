//! Concurrency regression coverage for `memory_ingest` requests sharing one
//! MCP session.
//!
//! Root-cause hypothesis: the shipped self-test helper (`mcp.sh call TOOL
//! REQUEST_ID ARGS`) passes an explicit JSON-RPC request id. Reusing the same
//! command concurrently sends several in-flight requests with the *same*
//! JSON-RPC id on one session. rmcp's session worker routes responses by
//! JSON-RPC id, so concurrent duplicates clobber each other: only one SSE
//! stream receives a response (and closes); the others wait forever.

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
    create_http_router, AuthConfig, FeatureFlags, HttpConfig, HttpRuntime, IdempotencyRepository,
    LimitsConfig, MemoryService, TokenAuthenticator, TokenConfig,
};
use memory_pipeline::error::Result as PipelineResult;
use memory_pipeline::extraction::{ExtractionBatch, MemoryExtractor, ModelUsage, SCHEMA_VERSION};
use memory_pipeline::grounding::{GroundingBatch, GroundingResult, GroundingVerifier};
use memory_pipeline::models::{AtomicMemory, ExtractionWindow, NormalizedMessage};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "alice-test-token";
const HOST: &str = "memory.example.test";

struct Fixture {
    app: Router,
    _temp: TempDir,
}

async fn fixture() -> Fixture {
    std::env::set_var("RAM_A_HTTP_MCP_TEST_TOKEN", TOKEN);
    let temp = tempfile::tempdir().unwrap();
    let database_path = temp.path().join("memory.sqlite");
    let memory_store = Arc::new(SqliteMemoryStore::new(&database_path));
    memory_store.initialize().await.unwrap();
    let manager = Arc::new(MemoryManager::new(
        memory_store,
        Arc::new(HashEmbedding::new(32)),
    ));
    let idempotency = IdempotencyRepository::open(&database_path).await.unwrap();
    let extractor: Arc<dyn MemoryExtractor> = Arc::new(SlowExtractor);
    let verifier: Arc<dyn GroundingVerifier> = Arc::new(SupportingVerifier);
    let service = MemoryService::new(manager, idempotency, extractor, verifier);
    let authenticator = TokenAuthenticator::from_config(&AuthConfig {
        tokens: vec![TokenConfig {
            token_env: "RAM_A_HTTP_MCP_TEST_TOKEN".to_string(),
            tenant_id: "tenant-a".to_string(),
            user_id: "alice".to_string(),
            agent_id: "agent-a".to_string(),
            permissions: vec!["memory:read".to_string(), "memory:write".to_string()],
        }],
    })
    .unwrap();
    let http = HttpConfig {
        allowed_origins: vec!["https://allowed.example".to_string()],
        allowed_hosts: vec![HOST.to_string()],
        ..HttpConfig::default()
    };
    let cancellation_token = tokio_util::sync::CancellationToken::new();
    let runtime = HttpRuntime::with_cancellation_token(
        service,
        Arc::new(authenticator),
        database_path.clone(),
        true,
        cancellation_token,
    )
    .with_features(FeatureFlags::all());
    Fixture {
        app: create_http_router(runtime, &http, &LimitsConfig::default()),
        _temp: temp,
    }
}

/// Extractor that takes long enough for all concurrent requests to be in
/// flight simultaneously before any of them completes.
struct SlowExtractor;

#[async_trait]
impl MemoryExtractor for SlowExtractor {
    fn model(&self) -> &str {
        "fixture"
    }
    fn prompt_version(&self) -> &str {
        "fixture-v1"
    }
    fn implementation(&self) -> &'static str {
        "SlowExtractor"
    }
    async fn extract(
        &self,
        window: &ExtractionWindow,
        messages: &HashMap<String, NormalizedMessage>,
    ) -> PipelineResult<ExtractionBatch> {
        tokio::time::sleep(Duration::from_millis(300)).await;
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

fn mcp_request(body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::HOST, HOST)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn initialize(app: &Router) -> String {
    let response = app
        .clone()
        .oneshot(mcp_request(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "memory-mcp-test", "version": "1"}
            }
        })))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

fn tools_call(session_id: &str, id: impl Into<Value>, conversation: &str) -> Request<Body> {
    let mut request = mcp_request(json!({
        "jsonrpc": "2.0",
        "id": id.into(),
        "method": "tools/call",
        "params": {
            "name": "memory_ingest",
            "arguments": {
                "conversation_id": conversation,
                "messages": [{"id": format!("message-{conversation}"), "role": "user", "text": "hello"}]
            }
        }
    }));
    request
        .headers_mut()
        .insert("mcp-session-id", session_id.parse().unwrap());
    request
        .headers_mut()
        .insert("mcp-protocol-version", "2025-11-25".parse().unwrap());
    request
}

async fn collect_response(response: Response) -> Value {
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

async fn concurrent_ingests(
    app: Router,
    session_id: String,
    ids: [i64; 3],
) -> Vec<Result<Value, StatusCode>> {
    let tasks = (0..3usize)
        .map(|i| {
            let app = app.clone();
            let session_id = session_id.clone();
            let request = tools_call(&session_id, ids[i], &format!("conversation-{i}"));
            tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(20), async move {
                    let response = app.oneshot(request).await.unwrap();
                    let status = response.status();
                    if status.is_success() {
                        Ok(collect_response(response).await)
                    } else {
                        Err(status)
                    }
                })
                .await
                .expect("request must complete instead of hanging")
            })
        })
        .collect::<Vec<_>>();
    let mut responses = Vec::new();
    for task in tasks {
        responses.push(task.await.unwrap());
    }
    responses
}

/// Three concurrent `memory_ingest` calls on one session with **distinct**
/// JSON-RPC ids — the contract the issue title promises.
#[tokio::test]
async fn three_concurrent_ingests_with_distinct_ids_all_complete() {
    let fixture = fixture().await;
    let session_id = initialize(&fixture.app).await;

    let responses = concurrent_ingests(fixture.app.clone(), session_id, [10, 11, 12]).await;
    for (index, response) in responses.iter().enumerate() {
        let response = response
            .as_ref()
            .expect("concurrent ingest with distinct ids should not hang");
        eprintln!("distinct-id response {index}: {response}");
        assert_eq!(
            response["result"]["structuredContent"]["accepted_count"],
            json!(1),
            "concurrent ingest {index} should succeed"
        );
    }
}

/// When a client reuses an explicit JSON-RPC id concurrently on one session,
/// rmcp's response routing cannot distinguish the requests and all but one
/// stream could hang forever. The middleware rejects duplicates with HTTP 409
/// instead.
#[tokio::test]
async fn three_concurrent_ingests_with_same_id_do_not_hang() {
    let fixture = fixture().await;
    let session_id = initialize(&fixture.app).await;

    let responses = concurrent_ingests(fixture.app.clone(), session_id, [42, 42, 42]).await;
    let succeeded = responses
        .iter()
        .filter(|response| {
            matches!(response, Ok(value)
                if value["result"]["structuredContent"]["accepted_count"] == json!(1))
        })
        .count();
    assert_eq!(
        succeeded, 1,
        "exactly one of the duplicate-id ingests should succeed"
    );
    for (index, response) in responses.iter().enumerate() {
        match response {
            Ok(value) => {
                assert_eq!(
                    value["result"]["structuredContent"]["accepted_count"],
                    json!(1),
                    "successful duplicate-id ingest {index}"
                );
            }
            Err(status) => {
                assert_eq!(
                    *status,
                    StatusCode::CONFLICT,
                    "duplicate in-flight id should fail fast with 409, got {status}"
                );
            }
        }
    }
}

/// Reusing an id is only rejected while the earlier request is still in
/// flight; once its response stream ends the id is free again.
#[tokio::test]
async fn same_id_reuse_after_completion_succeeds() {
    let fixture = fixture().await;
    let session_id = initialize(&fixture.app).await;

    let first = collect_response(
        fixture
            .app
            .clone()
            .oneshot(tools_call(&session_id, 7, "conversation-first"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        first["result"]["structuredContent"]["accepted_count"],
        json!(1)
    );

    let second = collect_response(
        fixture
            .app
            .clone()
            .oneshot(tools_call(&session_id, 7, "conversation-second"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        second["result"]["structuredContent"]["accepted_count"],
        json!(1),
        "id reuse after the first stream completed should succeed"
    );
}

/// Dropping an unconsumed response body must release the registration. This
/// models a client disconnect after the server has started streaming.
#[tokio::test]
async fn same_id_reuse_after_dropped_response_succeeds() {
    let fixture = fixture().await;
    let session_id = initialize(&fixture.app).await;
    let first = fixture
        .app
        .clone()
        .oneshot(tools_call(&session_id, 8, "conversation-cancelled"))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let duplicate = fixture
        .app
        .clone()
        .oneshot(tools_call(&session_id, 8, "conversation-duplicate"))
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);

    drop(first);
    let retried = tokio::time::timeout(
        Duration::from_secs(20),
        fixture
            .app
            .clone()
            .oneshot(tools_call(&session_id, 8, "conversation-retried")),
    )
    .await
    .expect("retry must not hang")
    .unwrap();
    assert_eq!(retried.status(), StatusCode::OK);
}

/// JSON-RPC numbers and strings are different id domains, so numeric `1` and
/// string `"1"` may be in flight concurrently on the same session.
#[tokio::test]
async fn numeric_and_string_ids_do_not_collide() {
    let fixture = fixture().await;
    let session_id = initialize(&fixture.app).await;

    let numeric = tokio::spawn(fixture.app.clone().oneshot(tools_call(
        &session_id,
        json!(1),
        "conversation-numeric",
    )));
    let string = tokio::spawn(fixture.app.clone().oneshot(tools_call(
        &session_id,
        json!("1"),
        "conversation-string",
    )));
    let (numeric, string) = tokio::join!(numeric, string);

    assert_eq!(numeric.unwrap().unwrap().status(), StatusCode::OK);
    assert_eq!(string.unwrap().unwrap().status(), StatusCode::OK);
}

/// Three concurrent requests for the *same* conversation and message content
/// with distinct ids: the ingest lock serializes them and the idempotency
/// table deduplicates, but every request must still answer.
#[tokio::test]
async fn three_concurrent_identical_ingests_with_distinct_ids_all_complete() {
    let fixture = fixture().await;
    let session_id = initialize(&fixture.app).await;

    let tasks = (0..3usize)
        .map(|i| {
            let app = fixture.app.clone();
            let session_id = session_id.clone();
            let request = tools_call(&session_id, 50 + i as i64, "conversation-same");
            tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(20), async move {
                    let response = app.oneshot(request).await.unwrap();
                    let status = response.status();
                    if status.is_success() {
                        Ok(collect_response(response).await)
                    } else {
                        Err(status)
                    }
                })
                .await
                .expect("request must complete instead of hanging")
            })
        })
        .collect::<Vec<_>>();
    for (index, task) in tasks.into_iter().enumerate() {
        let response = task
            .await
            .unwrap()
            .expect("identical ingest should succeed");
        assert_eq!(
            response["result"]["structuredContent"]["accepted_count"],
            json!(1),
            "identical concurrent ingest {index} should succeed (idempotent)"
        );
    }
}

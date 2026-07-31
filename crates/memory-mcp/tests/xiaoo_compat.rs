use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use axum::response::Response;
use axum::Router;
use memory_core::{HashEmbedding, MemoryManager, SqliteMemoryStore};
use memory_mcp::{
    create_http_router, AuthConfig, CaseReference, CaseSearchProvider, CaseSearchRequest,
    CaseSearchResponse, CaseServiceError, HttpConfig, HttpRuntime, IdempotencyRepository,
    LimitsConfig, MemoryService, Principal, TokenAuthenticator, TokenConfig, AGENT_ID_HEADER,
};
use memory_pipeline::error::Result as PipelineResult;
use memory_pipeline::extraction::{ExtractionBatch, MemoryExtractor, ModelUsage, SCHEMA_VERSION};
use memory_pipeline::grounding::{GroundingBatch, GroundingResult, GroundingVerifier};
use memory_pipeline::models::{AtomicMemory, ExtractionWindow, NormalizedMessage};
use serde_json::{json, Value};
use tower::ServiceExt;

const ALICE_AGENT_A_TOKEN: &str = "xiaoo-alice-agent-a-token";
const ALICE_AGENT_B_TOKEN: &str = "xiaoo-alice-agent-b-token";
const BOB_AGENT_TOKEN: &str = "xiaoo-bob-agent-token";

#[tokio::test]
async fn xiaoo_streamable_http_client_shares_memories_by_tenant_user_scope() {
    let temp = tempfile::tempdir().unwrap();
    let database_path = temp.path().join("memory.sqlite");
    let app = fixture_router(&database_path).await;

    let (alice_agent_a_session, initialized) =
        initialize(&app, ALICE_AGENT_A_TOKEN, "xiaoo-agent-a").await;
    assert_eq!(
        initialized["result"]["protocolVersion"],
        json!("2025-11-25")
    );

    let listed = call_session(
        &app,
        ALICE_AGENT_A_TOKEN,
        "xiaoo-agent-a",
        &alice_agent_a_session,
        2,
        json!({"method": "tools/list", "params": {}}),
    )
    .await;
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = response_json(listed).await;
    let mut tool_names = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    tool_names.sort();
    assert_eq!(
        tool_names,
        vec!["memory_case_search", "memory_ingest", "memory_search"]
    );

    let case_search = call_tool(
        &app,
        ALICE_AGENT_A_TOKEN,
        "xiaoo-agent-a",
        &alice_agent_a_session,
        8,
        "memory_case_search",
        json!({
            "query": "Wi-Fi 满格但 DNS 解析失败怎么处理？",
            "top_k": 3
        }),
    )
    .await;
    assert_eq!(case_search.status(), StatusCode::OK);
    let case_search = response_json(case_search).await;
    assert_eq!(case_search["result"]["isError"], json!(false));
    assert_eq!(
        case_search["result"]["structuredContent"]["library"],
        json!("ops")
    );
    assert_eq!(
        case_search["result"]["structuredContent"]["references"][0]["source_name"],
        json!("WiFi慢因为DNS解析异常.md")
    );

    let ingested = call_tool(
        &app,
        ALICE_AGENT_A_TOKEN,
        "xiaoo-agent-a",
        &alice_agent_a_session,
        3,
        "memory_ingest",
        json!({
            "conversation_id": "conversation-xiaoo-1",
            "messages": [
                {
                    "id": "message-context-1",
                    "role": "assistant",
                    "speaker": "xiaoO",
                    "text": "Noted for future travel planning.",
                    "timestamp": "2026-07-23T07:59:55Z",
                    "candidate": false
                },
                {
                    "id": "message-candidate-1",
                    "role": "user",
                    "speaker": "Alice",
                    "text": "I prefer a window seat when flying.",
                    "timestamp": "2026-07-23T08:00:00Z",
                    "candidate": true
                }
            ]
        }),
    )
    .await;
    assert_eq!(ingested.status(), StatusCode::OK);
    let ingested = response_json(ingested).await;
    assert_eq!(ingested["result"]["isError"], json!(false));
    assert_eq!(
        ingested["result"]["structuredContent"]["accepted_count"],
        json!(1)
    );
    assert_eq!(
        ingested["result"]["structuredContent"]["idempotency_hit"],
        json!(false)
    );

    let (alice_agent_b_session, _) = initialize(&app, ALICE_AGENT_B_TOKEN, "xiaoo-agent-b").await;
    let shared = search(
        &app,
        ALICE_AGENT_B_TOKEN,
        "xiaoo-agent-b",
        &alice_agent_b_session,
        4,
        "window seat",
    )
    .await;
    assert_eq!(shared["memories"].as_array().unwrap().len(), 1);
    assert_eq!(
        shared["memories"][0]["source_agent_id"],
        json!("xiaoo-agent-a")
    );

    let (bob_session, _) = initialize(&app, BOB_AGENT_TOKEN, "xiaoo-bob-agent").await;
    let isolated = search(
        &app,
        BOB_AGENT_TOKEN,
        "xiaoo-bob-agent",
        &bob_session,
        5,
        "window seat",
    )
    .await;
    assert!(isolated["memories"].as_array().unwrap().is_empty());

    drop(app);
    let restarted = fixture_router(&database_path).await;
    let (restarted_alice_agent_b_session, _) =
        initialize(&restarted, ALICE_AGENT_B_TOKEN, "xiaoo-agent-b").await;
    let persisted = search(
        &restarted,
        ALICE_AGENT_B_TOKEN,
        "xiaoo-agent-b",
        &restarted_alice_agent_b_session,
        6,
        "window seat",
    )
    .await;
    assert_eq!(persisted["memories"].as_array().unwrap().len(), 1);

    let (restarted_alice_agent_a_session, _) =
        initialize(&restarted, ALICE_AGENT_A_TOKEN, "xiaoo-agent-a").await;
    let repeated = call_tool(
        &restarted,
        ALICE_AGENT_A_TOKEN,
        "xiaoo-agent-a",
        &restarted_alice_agent_a_session,
        7,
        "memory_ingest",
        json!({
            "conversation_id": "conversation-xiaoo-1",
            "messages": [
                {
                    "id": "message-context-1",
                    "role": "assistant",
                    "speaker": "xiaoO",
                    "text": "Noted for future travel planning.",
                    "timestamp": "2026-07-23T07:59:55Z",
                    "candidate": false
                },
                {
                    "id": "message-candidate-1",
                    "role": "user",
                    "speaker": "Alice",
                    "text": "I prefer a window seat when flying.",
                    "timestamp": "2026-07-23T08:00:00Z",
                    "candidate": true
                }
            ]
        }),
    )
    .await;
    assert_eq!(repeated.status(), StatusCode::OK);
    let repeated = response_json(repeated).await;
    assert_eq!(repeated["result"]["isError"], json!(false));
    assert_eq!(
        repeated["result"]["structuredContent"]["idempotency_hit"],
        json!(true)
    );
    assert_eq!(
        repeated["result"]["structuredContent"]["accepted_count"],
        json!(1)
    );
}

struct StaticPreferenceExtractor;

#[async_trait]
impl MemoryExtractor for StaticPreferenceExtractor {
    fn model(&self) -> &str {
        "static-xiaoo-compat"
    }

    fn prompt_version(&self) -> &str {
        "static-xiaoo-compat-v1"
    }

    fn implementation(&self) -> &'static str {
        "StaticPreferenceExtractor"
    }

    async fn extract(
        &self,
        window: &ExtractionWindow,
        messages: &HashMap<String, NormalizedMessage>,
    ) -> PipelineResult<ExtractionBatch> {
        assert_eq!(window.candidate_refs.len(), 1);
        assert_eq!(
            window.candidate_message_ids,
            vec![window.candidate_refs[0].message_id.clone()]
        );
        let candidate_message = &messages[&window.candidate_refs[0].message_id];
        assert_eq!(
            candidate_message.text,
            "I prefer a window seat when flying."
        );
        assert_eq!(candidate_message.metadata["memory_candidate"], json!(true));
        let context_ref = window
            .context_before_refs
            .iter()
            .find(|reference| {
                messages[&reference.message_id].text == "Noted for future travel planning."
            })
            .expect("context-only message should be available as context");
        assert_eq!(
            messages[&context_ref.message_id].metadata["memory_candidate"],
            json!(false)
        );
        assert!(!window
            .candidate_refs
            .iter()
            .any(|reference| messages[&reference.message_id].text
                == "Noted for future travel planning."));
        assert!(!messages[&context_ref.message_id].candidate_eligible);
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

struct StaticSupportingVerifier;

#[async_trait]
impl GroundingVerifier for StaticSupportingVerifier {
    fn model(&self) -> &str {
        "static-xiaoo-compat"
    }

    fn prompt_version(&self) -> &str {
        "static-xiaoo-compat-v1"
    }

    fn implementation(&self) -> &'static str {
        "StaticSupportingVerifier"
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

struct StaticCaseSearchProvider;

#[async_trait]
impl CaseSearchProvider for StaticCaseSearchProvider {
    async fn search(
        &self,
        principal: &Principal,
        request: CaseSearchRequest,
    ) -> Result<CaseSearchResponse, CaseServiceError> {
        assert_eq!(principal.tenant_id, "tenant-a");
        assert_eq!(request.library, None);
        assert_eq!(request.top_k, 3);
        Ok(CaseSearchResponse {
            library: "ops".to_owned(),
            references: vec![CaseReference {
                chunk_id: "chunk-1".to_owned(),
                document_id: "document-1".to_owned(),
                source_name: Some("WiFi慢因为DNS解析异常.md".to_owned()),
                content: "刷新 DNS 缓存并检查 DNS 代理服务。".to_owned(),
                score: 0.82,
            }],
            truncated: false,
        })
    }
}

async fn fixture_router(database_path: &Path) -> Router {
    set_token_envs();
    let memory_store = Arc::new(SqliteMemoryStore::new(database_path));
    memory_store.initialize().await.unwrap();
    let manager = Arc::new(MemoryManager::new(
        memory_store,
        Arc::new(HashEmbedding::new(32)),
    ));
    let idempotency = IdempotencyRepository::open(database_path).await.unwrap();
    let extractor: Arc<dyn MemoryExtractor> = Arc::new(StaticPreferenceExtractor);
    let verifier: Arc<dyn GroundingVerifier> = Arc::new(StaticSupportingVerifier);
    let service = MemoryService::new(manager, idempotency, extractor, verifier);
    let authenticator = TokenAuthenticator::from_config(&AuthConfig {
        tokens: vec![
            token_config(
                "RAM_A_XIAOO_COMPAT_ALICE_AGENT_A",
                "tenant-a",
                "alice",
                "xiaoo-agent-a",
            ),
            token_config(
                "RAM_A_XIAOO_COMPAT_ALICE_AGENT_B",
                "tenant-a",
                "alice",
                "xiaoo-agent-b",
            ),
            token_config(
                "RAM_A_XIAOO_COMPAT_BOB_AGENT",
                "tenant-a",
                "bob",
                "xiaoo-bob-agent",
            ),
        ],
    })
    .unwrap();
    let runtime = HttpRuntime::with_cancellation_token(
        service,
        Arc::new(authenticator),
        database_path.to_path_buf(),
        true,
        tokio_util::sync::CancellationToken::new(),
    )
    .with_case_search_provider(Arc::new(StaticCaseSearchProvider));
    create_http_router(
        runtime,
        &HttpConfig {
            allowed_origins: vec!["https://xiaoo.example".to_string()],
            allowed_hosts: vec!["127.0.0.1".to_string()],
            ..HttpConfig::default()
        },
        &LimitsConfig::default(),
    )
}

fn set_token_envs() {
    std::env::set_var("RAM_A_XIAOO_COMPAT_ALICE_AGENT_A", ALICE_AGENT_A_TOKEN);
    std::env::set_var("RAM_A_XIAOO_COMPAT_ALICE_AGENT_B", ALICE_AGENT_B_TOKEN);
    std::env::set_var("RAM_A_XIAOO_COMPAT_BOB_AGENT", BOB_AGENT_TOKEN);
}

fn token_config(token_env: &str, tenant_id: &str, user_id: &str, agent_id: &str) -> TokenConfig {
    TokenConfig {
        token_env: token_env.to_string(),
        tenant_id: tenant_id.to_string(),
        user_id: user_id.to_string(),
        agent_id: agent_id.to_string(),
        permissions: vec![
            "memory:read".to_string(),
            "memory:write".to_string(),
            "cases:read".to_string(),
        ],
    }
}

async fn initialize(app: &Router, token: &str, agent_id: &str) -> (String, Value) {
    let response = app
        .clone()
        .oneshot(mcp_request(
            token,
            agent_id,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "xiaoO", "version": "test"}
                }
            }),
        ))
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

async fn search(
    app: &Router,
    token: &str,
    agent_id: &str,
    session_id: &str,
    id: u64,
    query: &str,
) -> Value {
    let response = call_tool(
        app,
        token,
        agent_id,
        session_id,
        id,
        "memory_search",
        json!({"query": query, "top_k": 5}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["result"]["isError"], json!(false));
    body["result"]["structuredContent"].clone()
}

async fn call_tool(
    app: &Router,
    token: &str,
    agent_id: &str,
    session_id: &str,
    id: u64,
    name: &str,
    arguments: Value,
) -> Response {
    call_session(
        app,
        token,
        agent_id,
        session_id,
        id,
        json!({"method": "tools/call", "params": {"name": name, "arguments": arguments}}),
    )
    .await
}

async fn call_session(
    app: &Router,
    token: &str,
    agent_id: &str,
    session_id: &str,
    id: u64,
    body: Value,
) -> Response {
    let mut body = body;
    body["jsonrpc"] = json!("2.0");
    body["id"] = json!(id);
    let mut request = mcp_request(token, agent_id, body);
    request
        .headers_mut()
        .insert("mcp-session-id", session_id.parse().unwrap());
    request
        .headers_mut()
        .insert("mcp-protocol-version", "2025-11-25".parse().unwrap());
    app.clone().oneshot(request).await.unwrap()
}

fn mcp_request(token: &str, agent_id: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::HOST, "127.0.0.1")
        .header(header::ORIGIN, "https://xiaoo.example")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(AGENT_ID_HEADER, agent_id)
        .body(Body::from(body.to_string()))
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

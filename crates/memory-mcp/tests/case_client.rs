use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use memory_mcp::{
    CaseLibraryConfig, CaseSearchProvider, CaseSearchRequest, CaseServiceClient, CaseServiceConfig,
    CaseServiceError, Principal,
};
use serde_json::{json, Value};

const CASE_TOKEN_ENV: &str = "RAM_A_CASE_CLIENT_TEST_TOKEN";
const CASE_TOKEN: &str = "case-service-secret";

#[derive(Clone)]
struct StubState {
    calls: Arc<AtomicUsize>,
}

async fn search_stub(
    State(state): State<StubState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some("Bearer case-service-secret")
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    assert_eq!(body, json!({"query": "DNS failure", "top_k": 3}));
    state.calls.fetch_add(1, Ordering::SeqCst);
    Ok(Json(json!({
        "chunks": [{
            "chunk_id": "chunk-1",
            "dataset_id": "openeuler-ops-cases",
            "document_id": "document-1",
            "source_name": "WiFi慢因为DNS解析异常.md",
            "source_path": "/private/server/path/WiFi慢因为DNS解析异常.md",
            "content": "刷新 DNS 缓存并检查 DNS 代理服务。",
            "score": 0.82
        }]
    })))
}

async fn unavailable_stub() -> StatusCode {
    StatusCode::SERVICE_UNAVAILABLE
}

async fn spawn_stub(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), task)
}

fn config(base_url: String) -> CaseServiceConfig {
    CaseServiceConfig {
        base_url,
        bearer_token_env: CASE_TOKEN_ENV.to_owned(),
        timeout_seconds: 1,
        max_response_bytes: 16_384,
        default_library: "ops".to_owned(),
        libraries: vec![CaseLibraryConfig {
            name: "ops".to_owned(),
            dataset_id: "openeuler-ops-cases".to_owned(),
            tenant_ids: vec!["tenant-a".to_owned()],
        }],
    }
}

fn principal(tenant_id: &str) -> Principal {
    Principal {
        tenant_id: tenant_id.to_owned(),
        user_id: "alice".to_owned(),
        agent_id: "xiaoo".to_owned(),
        permissions: vec!["cases:read".to_owned()],
    }
}

#[test]
fn client_rejects_noncanonical_internal_tokens_without_exposing_them() {
    const ENV_NAME: &str = "RAM_A_CASE_CLIENT_NONCANONICAL_TOKEN_TEST";
    const SECRET: &str = " private-case-secret ";
    std::env::set_var(ENV_NAME, SECRET);
    let mut client_config = config("http://127.0.0.1:1".to_owned());
    client_config.bearer_token_env = ENV_NAME.to_owned();

    let error = match CaseServiceClient::from_config(&client_config) {
        Ok(_) => panic!("noncanonical token must be rejected"),
        Err(error) => error,
    };

    std::env::remove_var(ENV_NAME);
    assert!(!error.to_string().contains(SECRET));
}

#[tokio::test]
async fn client_maps_private_library_to_dataset_and_redacts_server_paths() {
    std::env::set_var(CASE_TOKEN_ENV, CASE_TOKEN);
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/api/v1/datasets/openeuler-ops-cases/search",
            post(search_stub),
        )
        .with_state(StubState {
            calls: calls.clone(),
        });
    let (base_url, server) = spawn_stub(app).await;
    let client = CaseServiceClient::from_config(&config(base_url)).unwrap();

    let response = client
        .search(
            &principal("tenant-a"),
            CaseSearchRequest {
                query: "DNS failure".to_owned(),
                library: None,
                top_k: 3,
            },
        )
        .await
        .unwrap();

    server.abort();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(response.library, "ops");
    assert_eq!(response.references.len(), 1);
    assert_eq!(
        response.references[0].source_name.as_deref(),
        Some("WiFi慢因为DNS解析异常.md")
    );
    let serialized = serde_json::to_string(&response).unwrap();
    assert!(!serialized.contains("/private/server/path"));
    assert!(!serialized.contains("openeuler-ops-cases"));
}

#[tokio::test]
async fn client_rejects_unknown_libraries_and_tenant_crossing_before_http() {
    std::env::set_var(CASE_TOKEN_ENV, CASE_TOKEN);
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/api/v1/datasets/openeuler-ops-cases/search",
            post(search_stub),
        )
        .with_state(StubState {
            calls: calls.clone(),
        });
    let (base_url, server) = spawn_stub(app).await;
    let client = CaseServiceClient::from_config(&config(base_url)).unwrap();

    let unknown = client
        .search(
            &principal("tenant-a"),
            CaseSearchRequest {
                query: "DNS failure".to_owned(),
                library: Some("secret-library".to_owned()),
                top_k: 3,
            },
        )
        .await
        .unwrap_err();
    let crossing = client
        .search(
            &principal("tenant-b"),
            CaseSearchRequest {
                query: "DNS failure".to_owned(),
                library: None,
                top_k: 3,
            },
        )
        .await
        .unwrap_err();

    server.abort();
    assert_eq!(unknown, CaseServiceError::Forbidden);
    assert_eq!(crossing, CaseServiceError::Forbidden);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn client_maps_upstream_failure_to_retriable_tool_error() {
    std::env::set_var(CASE_TOKEN_ENV, CASE_TOKEN);
    let app = Router::new().route(
        "/api/v1/datasets/openeuler-ops-cases/search",
        post(unavailable_stub),
    );
    let (base_url, server) = spawn_stub(app).await;
    let client = CaseServiceClient::from_config(&config(base_url)).unwrap();

    let error = client
        .search(
            &principal("tenant-a"),
            CaseSearchRequest {
                query: "DNS failure".to_owned(),
                library: None,
                top_k: 3,
            },
        )
        .await
        .unwrap_err();

    server.abort();
    assert_eq!(error, CaseServiceError::Unavailable);
    assert_eq!(error.code(), "CASE_UNAVAILABLE");
    assert!(error.retriable());
}

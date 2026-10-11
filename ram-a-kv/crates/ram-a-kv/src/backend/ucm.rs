// UCM backend: prefetch/evict/query against the UCM KV prefetch chain.
//
// UCM exposes a single prefetch entrypoint through vllm-router:
//   POST {base_url}/kv/prefetch   body: {"block_ids": ["<32-hex>", ...]}
//                                 header: x-session-id: <session>
// The router reuses the same consistent-hash policy as inference, so a prefetch
// carrying the session's `x-session-id` lands on the same DP/TP ranks as that
// session's inference requests (constructive "prefetch == inference locality").
//
// UCM has no evict/query API: eviction is handled inside UCM's DDR buffer (LRU),
// so evict is a best-effort no-op and query reports nothing. This mirrors the
// semantics the coordinator relies on (evict is only invoked for chunks whose
// global refcount dropped to 0; UCM reclaims DDR on its own pressure).

use async_trait::async_trait;
use manager_core::backend::{BackendQueryResult, BackendResult, KvCacheBackend};
use std::collections::HashMap;
use std::time::Duration;

// Local service: 5s is enough to spot hangs; query has its own timeout so a hung
// upstream cannot block query callers indefinitely.
const PREFETCH_TIMEOUT: Duration = Duration::from_secs(5);
const EVICT_TIMEOUT: Duration = Duration::from_secs(5);
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

pub struct UCMBackend {
    // vllm-router base URL, e.g. "http://127.0.0.1:9003".
    base_url: String,
    client: reqwest::Client,
}

impl UCMBackend {
    pub fn new(base_url: String) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("curl/7.79.1")
            .http1_title_case_headers()
            .build()
            .expect("failed to build reqwest client");
        Self { base_url, client }
    }

    #[allow(dead_code)]
    pub fn default_local() -> Self {
        Self::new("http://127.0.0.1:9003".to_string())
    }
}

#[async_trait]
impl KvCacheBackend for UCMBackend {
    async fn prefetch(&self, hashes: Vec<String>, lookup_id: String, session_id: String) -> BackendResult {
        // The router rejects an empty block_ids array (400); skip the round-trip.
        if hashes.is_empty() {
            return BackendResult {
                status: 204,
                responded: true,
            };
        }
        let payload = serde_json::json!({ "block_ids": hashes });
        let count = hashes.len();
        match self
            .client
            .post(format!("{}/kv/prefetch", self.base_url))
            .header("Content-Type", "application/json")
            .header("Accept", "*/*")
            .header("x-session-id", session_id.as_str())
            .timeout(PREFETCH_TIMEOUT)
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                tracing::info!(
                    status = status,
                    count = count,
                    lookup_id = %lookup_id,
                    session_id = %session_id,
                    response = %body,
                    "UCM prefetch response"
                );
                BackendResult {
                    status,
                    responded: true,
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    count = count,
                    lookup_id = %lookup_id,
                    session_id = %session_id,
                    "UCM prefetch request failed"
                );
                BackendResult {
                    status: 0,
                    responded: false,
                }
            }
        }
    }

    // UCM has no explicit evict endpoint; DDR reclamation is internal (LRU on
    // buffer pressure). Report success so the coordinator's refcount bookkeeping
    // stays consistent and does not mark the backend degraded.
    async fn evict(&self, hashes: Vec<String>) -> BackendResult {
        let count = hashes.len();
        let _ = EVICT_TIMEOUT;
        tracing::debug!(
            count = count,
            "UCM has no evict API; treating evict as a no-op"
        );
        BackendResult {
            status: 204,
            responded: true,
        }
    }

    // UCM exposes no location-query API.
    async fn query(&self, hashes: Vec<String>) -> BackendQueryResult {
        let _ = (hashes, QUERY_TIMEOUT);
        BackendQueryResult {
            locations: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{State as AxumState, Json as AxumJson};
    use axum::http::{HeaderMap, Method, StatusCode};
    use axum::routing::post;
    use axum::Router;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Captured {
        method: String,
        path: String,
        session_id: Option<String>,
        content_type: Option<String>,
        body: Option<serde_json::Value>,
    }

    struct MockState {
        captured: Mutex<Captured>,
        hits: AtomicUsize,
        response: (StatusCode, serde_json::Value),
    }

    // Start an axum mock of the UCM router's POST /kv/prefetch and return its base URL.
    async fn spawn_mock(response: (StatusCode, serde_json::Value)) -> (String, Arc<MockState>) {
        let state = Arc::new(MockState {
            captured: Mutex::new(Captured::default()),
            hits: AtomicUsize::new(0),
            response,
        });

        async fn handler(
            AxumState(state): AxumState<Arc<MockState>>,
            method: Method,
            uri: axum::http::Uri,
            headers: HeaderMap,
            AxumJson(body): AxumJson<serde_json::Value>,
        ) -> (StatusCode, AxumJson<serde_json::Value>) {
            state.hits.fetch_add(1, Ordering::SeqCst);
            {
                let mut cap = state.captured.lock().unwrap();
                cap.method = method.as_str().to_string();
                cap.path = uri.path().to_string();
                cap.session_id = headers
                    .get("x-session-id")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                cap.content_type = headers
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                cap.body = Some(body);
            }
            (state.response.0, AxumJson(state.response.1.clone()))
        }

        let app = Router::new()
            .route("/kv/prefetch", post(handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{}", addr), state)
    }

    #[tokio::test]
    async fn prefetch_sends_block_ids_and_session_header() {
        let (base, state) =
            spawn_mock((StatusCode::OK, serde_json::json!({"ok": true, "count": 2}))).await;
        let backend = UCMBackend::new(base);

        let result = backend
            .prefetch(
                vec!["aa".repeat(16), "bb".repeat(16)],
                "turn_prefetch_sess-1".to_string(),
                "sess-1".to_string(),
            )
            .await;

        assert!(result.responded);
        assert_eq!(result.status, 200);
        assert_eq!(state.hits.load(Ordering::SeqCst), 1);
        let cap = state.captured.lock().unwrap();
        assert_eq!(cap.method, "POST");
        assert_eq!(cap.path, "/kv/prefetch");
        assert_eq!(cap.session_id.as_deref(), Some("sess-1"));
        assert!(cap
            .content_type
            .as_deref()
            .unwrap_or_default()
            .starts_with("application/json"));
        assert_eq!(
            cap.body,
            Some(serde_json::json!({"block_ids": ["aa".repeat(16), "bb".repeat(16)]}))
        );
    }

    #[tokio::test]
    async fn prefetch_empty_hashes_skips_round_trip() {
        let (base, state) =
            spawn_mock((StatusCode::OK, serde_json::json!({"ok": true, "count": 0}))).await;
        let backend = UCMBackend::new(base);

        let result = backend
            .prefetch(vec![], "turn_prefetch_s".to_string(), "s".to_string())
            .await;

        assert!(result.responded);
        assert_eq!(result.status, 204);
        assert_eq!(state.hits.load(Ordering::SeqCst), 0, "no HTTP call for empty hashes");
    }

    #[tokio::test]
    async fn prefetch_maps_upstream_error_status_without_degrading_responded() {
        // Router returns 502 when every TP rank failed: responded=true, non-2xx.
        let (base, _state) = spawn_mock((
            StatusCode::BAD_GATEWAY,
            serde_json::json!({"ok": false, "mode": "router"}),
        ))
        .await;
        let backend = UCMBackend::new(base);

        let result = backend
            .prefetch(
                vec!["cc".repeat(16)],
                "turn_prefetch_s".to_string(),
                "s".to_string(),
            )
            .await;

        assert!(result.responded);
        assert_eq!(result.status, 502);
    }

    #[tokio::test]
    async fn prefetch_network_error_reports_not_responded() {
        // Nothing is listening on this port.
        let backend = UCMBackend::new("http://127.0.0.1:1".to_string());

        let result = backend
            .prefetch(
                vec!["dd".repeat(16)],
                "turn_prefetch_s".to_string(),
                "s".to_string(),
            )
            .await;

        assert!(!result.responded);
        assert_eq!(result.status, 0);
    }

    #[tokio::test]
    async fn evict_and_query_are_noops() {
        let backend = UCMBackend::new("http://127.0.0.1:1".to_string());

        let evict = backend.evict(vec!["ee".repeat(16)]).await;
        assert!(evict.responded);
        assert_eq!(evict.status, 204);

        let query = backend.query(vec!["ee".repeat(16)]).await;
        assert!(query.locations.is_empty());
    }
}

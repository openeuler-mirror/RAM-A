use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{MemoryError, MemoryResult, RerankConfig, ScoredMemory};

const RERANK_MAX_ATTEMPTS: usize = 8;

fn retry_backoff(attempt: usize) -> Duration {
    Duration::from_secs(1 << (attempt - 1))
}

/// Whether a rerank failure is worth retrying (network drop, retryable HTTP status,
/// decode hiccup). Mirrors the embedding retry policy.
fn is_retryable_rerank_failure(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("error sending request")
        || lower.contains("failed to read")
        || lower.contains("operation timed out")
        || lower.contains("decode failed")
        || lower.contains("http 408")
        || lower.contains("http 425")
        || lower.contains("http 429")
        || lower.contains("http 500")
        || lower.contains("http 502")
        || lower.contains("http 503")
        || lower.contains("http 504")
}

#[async_trait]
pub trait Reranker: Send + Sync {
    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<ScoredMemory>,
        top_k: usize,
    ) -> MemoryResult<Vec<ScoredMemory>>;
}

pub struct OpenRouterReranker {
    client: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
    model: String,
    timeout: Option<Duration>,
}

impl OpenRouterReranker {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::with_base_url(api_key, "https://openrouter.ai/api/v1", model)
    }

    pub fn with_base_url(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: Some(api_key.into()),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            timeout: None,
        }
    }

    pub fn from_config(api_key: impl Into<String>, config: &RerankConfig) -> Self {
        Self::with_base_url(api_key, config.base_url.clone(), config.model.clone())
            .with_timeout_ms(config.timeout_ms)
    }

    /// Construct an OpenRouter-wire-compatible reranker. `None` is intended for
    /// trusted local endpoints that do not require Bearer authentication.
    pub fn from_config_with_optional_api_key(
        api_key: Option<String>,
        config: &RerankConfig,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: config.base_url.trim_end_matches('/').to_string(),
            model: config.model.clone(),
            timeout: config.timeout_ms.map(Duration::from_millis),
        }
    }

    pub fn with_timeout_ms(mut self, timeout_ms: Option<u64>) -> Self {
        self.timeout = timeout_ms.map(Duration::from_millis);
        self
    }

    fn rerank_url(&self) -> String {
        if self.base_url.ends_with("/rerank") {
            self.base_url.clone()
        } else {
            format!("{}/rerank", self.base_url)
        }
    }
}

#[async_trait]
impl Reranker for OpenRouterReranker {
    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<ScoredMemory>,
        top_k: usize,
    ) -> MemoryResult<Vec<ScoredMemory>> {
        if top_k == 0 || candidates.is_empty() {
            return Ok(Vec::new());
        }

        let top_n = top_k.min(candidates.len());
        let body = openrouter_request_body(&self.model, query, &candidates, top_n);

        let mut last_error = None;
        for attempt in 1..=RERANK_MAX_ATTEMPTS {
            let mut request = self.client.post(self.rerank_url()).json(&body);
            if let Some(api_key) = self.api_key.as_deref() {
                request = request.bearer_auth(api_key);
            }
            if let Some(timeout) = self.timeout {
                request = request.timeout(timeout);
            }

            let send_result = request.send().await;
            let attempt_error = match send_result {
                Ok(response) => {
                    let status = response.status();
                    let body_text = response.text().await.map_err(|error| {
                        format!("failed to read OpenRouter rerank response body: {error}")
                    });
                    match body_text {
                        Err(msg) => Some(msg),
                        Ok(body_text) if !status.is_success() => Some(format!(
                            "OpenRouter rerank API returned HTTP {status}: {}",
                            preview_body(&body_text)
                        )),
                        Ok(body_text) => match serde_json::from_str::<OpenRouterRerankResponse>(
                            &body_text,
                        ) {
                            Ok(parsed) => {
                                return apply_openrouter_results(candidates, parsed.results, top_n);
                            }
                            Err(error) => Some(format!(
                                "decode failed for OpenRouter rerank response: {error}; body preview: {}",
                                preview_body(&body_text)
                            )),
                        },
                    }
                }
                Err(error) => Some(format!("OpenRouter rerank request failed: {error}")),
            };

            if let Some(message) = attempt_error {
                let retryable = is_retryable_rerank_failure(&message);
                last_error = Some(message.clone());
                if retryable && attempt < RERANK_MAX_ATTEMPTS {
                    let backoff = retry_backoff(attempt);
                    tracing::warn!(
                        event = "ram_a.provider.retry",
                        provider_kind = "rerank",
                        provider = "openrouter_compatible",
                        model = self.model,
                        operation = "rerank",
                        attempt,
                        max_attempts = RERANK_MAX_ATTEMPTS,
                        backoff_ms = backoff.as_millis() as u64,
                        error_kind = retry_error_kind(&message)
                    );
                    tokio::time::sleep(backoff).await;
                    continue;
                }

                tracing::error!(
                    event = "ram_a.provider.failed",
                    provider_kind = "rerank",
                    provider = "openrouter_compatible",
                    model = self.model,
                    operation = "rerank",
                    attempts = attempt,
                    error_kind = retry_error_kind(&message)
                );
                return Err(MemoryError::Rerank { message });
            }
        }

        Err(MemoryError::Rerank {
            message: last_error
                .unwrap_or_else(|| "OpenRouter rerank request failed without an error".to_string()),
        })
    }
}

fn retry_error_kind(message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    for (needle, kind) in [
        ("http 429", "http_429"),
        ("http 503", "http_503"),
        ("http 502", "http_502"),
        ("http 504", "http_504"),
        ("timed out", "timeout"),
        ("decode failed", "decode"),
        ("failed to read", "read"),
    ] {
        if lower.contains(needle) {
            return kind;
        }
    }
    "request"
}

#[derive(Serialize)]
struct OpenRouterRerankRequest<'a> {
    model: &'a str,
    query: &'a str,
    documents: Vec<&'a str>,
    top_n: usize,
}

fn openrouter_request_body<'a>(
    model: &'a str,
    query: &'a str,
    candidates: &'a [ScoredMemory],
    top_n: usize,
) -> OpenRouterRerankRequest<'a> {
    OpenRouterRerankRequest {
        model,
        query,
        documents: candidates
            .iter()
            .map(|candidate| candidate.record.text.as_str())
            .collect(),
        top_n,
    }
}

#[derive(Debug, Deserialize)]
struct OpenRouterRerankResponse {
    results: Vec<OpenRouterRerankResult>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterRerankResult {
    index: usize,
    relevance_score: f32,
}

fn apply_openrouter_results(
    candidates: Vec<ScoredMemory>,
    results: Vec<OpenRouterRerankResult>,
    top_n: usize,
) -> MemoryResult<Vec<ScoredMemory>> {
    let expected = top_n.min(candidates.len());
    if results.len() < expected {
        return Err(MemoryError::Rerank {
            message: format!(
                "OpenRouter rerank response returned {} results for top_n {expected}",
                results.len()
            ),
        });
    }

    let mut seen_indexes = HashSet::new();
    let mut ranked = Vec::with_capacity(expected);
    for result in results.into_iter().take(expected) {
        if result.index >= candidates.len() {
            return Err(MemoryError::Rerank {
                message: format!(
                    "OpenRouter rerank response index {} out of range for {} candidates",
                    result.index,
                    candidates.len()
                ),
            });
        }
        if !seen_indexes.insert(result.index) {
            return Err(MemoryError::Rerank {
                message: format!(
                    "OpenRouter rerank response returned duplicate index {}",
                    result.index
                ),
            });
        }
        if !result.relevance_score.is_finite() {
            return Err(MemoryError::Rerank {
                message: format!(
                    "OpenRouter rerank response returned non-finite score for index {}",
                    result.index
                ),
            });
        }

        let mut candidate = candidates[result.index].clone();
        candidate.score = result.relevance_score;
        ranked.push(candidate);
    }

    Ok(ranked)
}

fn preview_body(body: &str) -> String {
    body.chars().take(1000).collect()
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    use super::*;
    use crate::MemoryRecord;

    #[test]
    fn retry_budget_covers_extended_network_outages() {
        assert_eq!(RERANK_MAX_ATTEMPTS, 8);
        assert_eq!(retry_backoff(7), Duration::from_secs(64));
    }

    fn candidate(id: &str) -> ScoredMemory {
        ScoredMemory {
            record: MemoryRecord {
                id: id.to_string(),
                text: format!("{id} text"),
                metadata: serde_json::json!({}),
                embedding: vec![0.0],
                created_at_ms: 10,
                updated_at_ms: 10,
            },
            score: 0.0,
        }
    }

    #[test]
    fn openrouter_results_map_indexes_to_original_candidates() {
        let results = apply_openrouter_results(
            vec![candidate("first"), candidate("second")],
            vec![OpenRouterRerankResult {
                index: 1,
                relevance_score: 0.91,
            }],
            1,
        )
        .expect("mapped rerank results");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].record.id, "second");
        assert!((results[0].score - 0.91).abs() < 0.0001);
    }

    #[test]
    fn local_openrouter_compatible_reranker_can_omit_bearer_auth() {
        let config = RerankConfig {
            enabled: true,
            base_url: "http://127.0.0.1:8081/v1".to_string(),
            model: "local-reranker".to_string(),
            ..RerankConfig::default()
        };
        let reranker = OpenRouterReranker::from_config_with_optional_api_key(None, &config);

        assert!(reranker.api_key.is_none());
        assert_eq!(reranker.rerank_url(), "http://127.0.0.1:8081/v1/rerank");
    }

    #[tokio::test]
    async fn unauthenticated_local_request_omits_authorization_header() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test reranker");
        let address = listener.local_addr().expect("test reranker address");
        let (request_tx, request_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept rerank request");
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = stream.read(&mut buffer).expect("read rerank request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            request_tx.send(request).expect("capture rerank request");
            let response_body = r#"{"results":[{"index":0,"relevance_score":0.9}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            )
            .expect("write rerank response");
        });
        let config = RerankConfig {
            enabled: true,
            base_url: format!("http://{address}"),
            model: "local-reranker".to_string(),
            ..RerankConfig::default()
        };
        let reranker = OpenRouterReranker::from_config_with_optional_api_key(None, &config);

        let results = reranker
            .rerank("query", vec![candidate("first")], 1)
            .await
            .expect("local rerank succeeds");
        server.join().expect("join test reranker");
        let request = String::from_utf8(request_rx.recv().expect("captured request"))
            .expect("request is UTF-8");

        assert_eq!(results.len(), 1);
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        assert!(request.starts_with("POST /rerank HTTP/1.1"));
    }

    #[test]
    fn openrouter_results_reject_out_of_range_index() {
        let error = apply_openrouter_results(
            vec![candidate("first")],
            vec![OpenRouterRerankResult {
                index: 1,
                relevance_score: 0.91,
            }],
            1,
        )
        .expect_err("out of range index should fail");

        assert!(format!("{error}").contains("out of range"));
    }

    #[test]
    fn openrouter_request_body_uses_model_query_documents_and_top_n() {
        let candidates = [candidate("first"), candidate("second")];
        let body =
            openrouter_request_body("cohere/rerank-v3.5", "coffee preference", &candidates, 1);
        let json = serde_json::to_value(body).expect("serialize request body");

        assert_eq!(
            json,
            serde_json::json!({
                "model": "cohere/rerank-v3.5",
                "query": "coffee preference",
                "documents": ["first text", "second text"],
                "top_n": 1,
            })
        );
    }
}

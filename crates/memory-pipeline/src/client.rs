use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::error::{PipelineError, Result};
use crate::extraction::ModelUsage;

#[derive(Clone)]
pub struct OpenAiCompatibleClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    max_retries: usize,
}

pub struct ChatResult {
    pub content: String,
    pub usage: ModelUsage,
}

impl OpenAiCompatibleClient {
    pub fn new(
        api_key: impl Into<String>,
        base_url: &str,
        timeout_seconds: u64,
        max_retries: usize,
    ) -> Result<Self> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(PipelineError::InvalidInput(
                "API key must not be empty".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_seconds))
            .build()
            .map_err(|error| {
                PipelineError::Protocol(format!("cannot build HTTP client: {error}"))
            })?;
        Ok(Self {
            client,
            api_key,
            base_url: base_url.trim_end_matches('/').into(),
            max_retries,
        })
    }

    pub fn from_env(
        api_key_env: &str,
        base_url: &str,
        timeout_seconds: u64,
        max_retries: usize,
    ) -> Result<Self> {
        let api_key = std::env::var(api_key_env).map_err(|_| {
            PipelineError::InvalidInput(format!("missing API key env {api_key_env}"))
        })?;
        Self::new(api_key, base_url, timeout_seconds, max_retries)
    }

    pub async fn chat(
        &self,
        model: &str,
        messages: Vec<Value>,
        max_tokens: usize,
    ) -> Result<ChatResult> {
        let payload = json!({"model": model, "messages": messages, "temperature": 0.0, "max_tokens": max_tokens});
        let mut last_error = String::new();
        for attempt in 0..self.max_retries.max(1) {
            let started = Instant::now();
            match self
                .client
                .post(format!("{}/chat/completions", self.base_url))
                .bearer_auth(&self.api_key)
                .json(&payload)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    let body = match response.text().await {
                        Ok(body) => body,
                        Err(error) => {
                            last_error = format!("chat completion body read failed: {error}");
                            if attempt + 1 < self.max_retries.max(1) {
                                tokio::time::sleep(Duration::from_secs(
                                    (1u64 << attempt.min(6)).min(64),
                                ))
                                .await;
                            }
                            continue;
                        }
                    };
                    let raw: Value = match serde_json::from_str(&body) {
                        Ok(raw) => raw,
                        Err(error) => {
                            last_error = format!("chat completion returned invalid JSON: {error}");
                            if attempt + 1 < self.max_retries.max(1) {
                                tokio::time::sleep(Duration::from_secs(
                                    (1u64 << attempt.min(6)).min(64),
                                ))
                                .await;
                            }
                            continue;
                        }
                    };
                    let content = raw
                        .pointer("/choices/0/message/content")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .to_owned();
                    if content.is_empty() {
                        last_error = "chat completion returned empty content".into();
                    } else {
                        let prompt = raw
                            .pointer("/usage/prompt_tokens")
                            .and_then(Value::as_i64)
                            .unwrap_or_else(|| estimate_value_tokens(&payload["messages"]));
                        let completion = raw
                            .pointer("/usage/completion_tokens")
                            .and_then(Value::as_i64)
                            .unwrap_or_else(|| estimate_text_tokens(&content));
                        let total = raw
                            .pointer("/usage/total_tokens")
                            .and_then(Value::as_i64)
                            .unwrap_or(prompt + completion);
                        return Ok(ChatResult {
                            content,
                            usage: ModelUsage {
                                latency_ms: started.elapsed().as_secs_f64() * 1000.0,
                                prompt_tokens: prompt,
                                completion_tokens: completion,
                                total_tokens: total,
                            },
                        });
                    }
                }
                Ok(response) => {
                    let status = response.status();
                    last_error = format!("HTTP {status}");
                    if !retryable(status) {
                        break;
                    }
                }
                Err(error) => {
                    last_error = error.to_string();
                }
            }
            if attempt + 1 < self.max_retries.max(1) {
                tokio::time::sleep(Duration::from_secs((1u64 << attempt.min(6)).min(64))).await;
            }
        }
        Err(PipelineError::Protocol(format!(
            "chat completion failed after retries: {last_error}"
        )))
    }
}

fn retryable(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

fn estimate_value_tokens(value: &Value) -> i64 {
    estimate_text_tokens(&value.to_string())
}
fn estimate_text_tokens(value: &str) -> i64 {
    ((value.chars().count() / 4).max(1)) as i64
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    #[test]
    fn rejects_empty_api_key() {
        assert!(OpenAiCompatibleClient::new("  ", "http://localhost", 1, 1).is_err());
    }

    #[tokio::test]
    async fn retries_invalid_success_json() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for body in [
                "not-json".to_owned(),
                serde_json::json!({
                    "choices": [{"message": {"content": "ok"}}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                })
                .to_string(),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });
        let client =
            OpenAiCompatibleClient::new("key", &format!("http://{address}"), 2, 2).unwrap();

        let result = client
            .chat("model", vec![json!({"role": "user", "content": "hi"})], 10)
            .await
            .unwrap();

        assert_eq!(result.content, "ok");
        assert_eq!(result.usage.total_tokens, 2);
        server.join().unwrap();
    }
}

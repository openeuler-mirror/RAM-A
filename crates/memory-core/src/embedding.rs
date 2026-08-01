use async_trait::async_trait;
use reqwest::StatusCode;
use serde::Deserialize;
use std::time::Duration;

use crate::{MemoryError, MemoryResult};

const EMBEDDING_MAX_ATTEMPTS: usize = 8;

#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    fn dimensions(&self) -> usize;

    fn model_name(&self) -> &str {
        "unknown-embedding-model"
    }

    fn profile_id(&self) -> String {
        format!("{}:{}", self.model_name(), self.dimensions())
    }

    async fn embed(&self, texts: &[String]) -> MemoryResult<Vec<Vec<f32>>>;

    async fn embed_one(&self, text: &str) -> MemoryResult<Vec<f32>> {
        let mut embeddings = self.embed(&[text.to_string()]).await?;
        embeddings.pop().ok_or_else(|| MemoryError::Embedding {
            message: "embedding provider returned no vectors".to_string(),
        })
    }
}

pub struct OpenRouterEmbedding {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    dimensions: usize,
}

impl OpenRouterEmbedding {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>, dimensions: usize) -> Self {
        Self::with_base_url(api_key, "https://openrouter.ai/api/v1", model, dimensions)
    }

    pub fn with_base_url(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        dimensions: usize,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            dimensions,
        }
    }

    fn embeddings_url(&self) -> String {
        if self.base_url.ends_with("/embeddings") {
            self.base_url.clone()
        } else {
            format!("{}/embeddings", self.base_url)
        }
    }

    async fn embed_once(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingAttemptError> {
        let response = self
            .client
            .post(self.embeddings_url())
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": self.model,
                "input": texts,
                "encoding_format": "float"
            }))
            .send()
            .await
            .map_err(|error| {
                EmbeddingAttemptError::retryable(format!("embedding request failed: {error}"))
            })?;

        let status = response.status();
        let body_text = response.text().await.map_err(|error| {
            EmbeddingAttemptError::retryable(format!(
                "failed to read embedding response body: {error}"
            ))
        })?;
        if !status.is_success() {
            let message = format!(
                "embedding API returned HTTP {status}: {}",
                preview_body(&body_text)
            );
            return Err(EmbeddingAttemptError {
                retryable: is_retryable_status(status),
                message,
            });
        }

        let body: EmbeddingResponse = serde_json::from_str(&body_text).map_err(|error| {
            EmbeddingAttemptError::retryable(format!(
                "decode failed for embedding response: {error}; body preview: {}",
                preview_body(&body_text)
            ))
        })?;

        let embeddings = ordered_embeddings(body.data).map_err(|error| {
            EmbeddingAttemptError::retryable(format!("invalid embedding response: {error}"))
        })?;
        for (index, vector) in embeddings.iter().enumerate() {
            if vector.len() != self.dimensions {
                return Err(EmbeddingAttemptError {
                    retryable: false,
                    message: format!(
                        "embedding dimension mismatch at index {}: expected {} got {}",
                        index,
                        self.dimensions,
                        vector.len()
                    ),
                });
            }
        }
        Ok(embeddings)
    }
}

#[async_trait]
impl EmbeddingProvider for OpenRouterEmbedding {
    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn profile_id(&self) -> String {
        format!(
            "openai_compatible:{}:{}:{}",
            self.base_url,
            self.model,
            self.dimensions()
        )
    }

    async fn embed(&self, texts: &[String]) -> MemoryResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        for attempt in 1..=EMBEDDING_MAX_ATTEMPTS {
            match self.embed_once(texts).await {
                Ok(embeddings) => return Ok(embeddings),
                Err(error) if error.retryable && attempt < EMBEDDING_MAX_ATTEMPTS => {
                    let backoff = retry_backoff(attempt);
                    eprintln!(
                        "OpenRouter embedding attempt {attempt}/{EMBEDDING_MAX_ATTEMPTS} failed: {}; retrying in {}s",
                        error.summary(),
                        backoff.as_secs()
                    );
                    tokio::time::sleep(backoff).await;
                }
                Err(error) => {
                    return Err(MemoryError::Embedding {
                        message: error.message,
                    });
                }
            }
        }

        Err(MemoryError::Embedding {
            message: "embedding request failed without an error".to_string(),
        })
    }
}

struct EmbeddingAttemptError {
    retryable: bool,
    message: String,
}

impl EmbeddingAttemptError {
    fn retryable(message: String) -> Self {
        Self {
            retryable: true,
            message,
        }
    }

    fn summary(&self) -> String {
        preview_body(&self.message)
    }
}

#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingItem>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingItem {
    index: Option<usize>,
    embedding: Vec<f32>,
}

fn ordered_embeddings(items: Vec<EmbeddingItem>) -> MemoryResult<Vec<Vec<f32>>> {
    let indexed_count = items.iter().filter(|item| item.index.is_some()).count();
    if indexed_count == 0 {
        Ok(items.into_iter().map(|item| item.embedding).collect())
    } else if indexed_count == items.len() {
        let mut indexed = items
            .into_iter()
            .map(|item| (item.index.unwrap_or_default(), item.embedding))
            .collect::<Vec<_>>();
        indexed.sort_by_key(|(index, _)| *index);

        let has_duplicate_or_gap = indexed
            .iter()
            .enumerate()
            .any(|(expected, (actual, _))| expected != *actual);
        if has_duplicate_or_gap {
            return Err(MemoryError::Embedding {
                message: "embedding API returned non-contiguous indexes".to_string(),
            });
        }

        Ok(indexed
            .into_iter()
            .map(|(_, embedding)| embedding)
            .collect())
    } else {
        Err(MemoryError::Embedding {
            message: "embedding API returned partial indexes".to_string(),
        })
    }
}

fn preview_body(body: &str) -> String {
    body.chars().take(1000).collect()
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn retry_backoff(attempt: usize) -> Duration {
    Duration::from_secs(1 << (attempt - 1))
}

/// Deterministic local embedding for tests and offline smoke checks.
/// It is not intended for benchmark scoring.
pub struct HashEmbedding {
    dimensions: usize,
}

impl HashEmbedding {
    pub fn new(dimensions: usize) -> Self {
        Self { dimensions }
    }
}

#[async_trait]
impl EmbeddingProvider for HashEmbedding {
    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn model_name(&self) -> &str {
        "hash-embedding"
    }

    fn profile_id(&self) -> String {
        format!("hash:{}", self.dimensions())
    }

    async fn embed(&self, texts: &[String]) -> MemoryResult<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| hash_embed(text, self.dimensions))
            .collect())
    }
}

fn hash_embed(text: &str, dimensions: usize) -> Vec<f32> {
    let mut vector = vec![0.0; dimensions.max(1)];
    for token in text.split_whitespace() {
        let mut hash = 1469598103934665603_u64;
        for byte in token.to_ascii_lowercase().bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(1099511628211);
        }
        let index = (hash as usize) % vector.len();
        vector[index] += 1.0;
    }
    vector
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_budget_covers_extended_network_outages() {
        assert_eq!(EMBEDDING_MAX_ATTEMPTS, 8);
        assert_eq!(retry_backoff(7), Duration::from_secs(64));
    }

    #[test]
    fn ordered_embeddings_sorts_by_provider_index() {
        let embeddings = ordered_embeddings(vec![
            EmbeddingItem {
                index: Some(1),
                embedding: vec![1.0],
            },
            EmbeddingItem {
                index: Some(0),
                embedding: vec![0.0],
            },
        ])
        .expect("ordered embeddings");

        assert_eq!(embeddings, vec![vec![0.0], vec![1.0]]);
    }

    #[test]
    fn ordered_embeddings_rejects_duplicate_or_missing_indexes() {
        let error = ordered_embeddings(vec![
            EmbeddingItem {
                index: Some(0),
                embedding: vec![0.0],
            },
            EmbeddingItem {
                index: Some(0),
                embedding: vec![1.0],
            },
        ])
        .expect_err("duplicate indexes should fail");

        assert!(format!("{error}").contains("non-contiguous indexes"));
    }

    #[test]
    fn ordered_embeddings_rejects_partial_indexes() {
        let error = ordered_embeddings(vec![
            EmbeddingItem {
                index: Some(0),
                embedding: vec![0.0],
            },
            EmbeddingItem {
                index: None,
                embedding: vec![1.0],
            },
        ])
        .expect_err("partial indexes should fail");

        assert!(format!("{error}").contains("partial indexes"));
    }
}

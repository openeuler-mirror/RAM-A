// LMCache-Ascend backend: real prefetch/evict/query over HTTP against the
// Huawei Ascend NPU KV-cache management service.

use async_trait::async_trait;
use manager_core::backend::{BackendQueryResult, BackendResult, KvCacheBackend};
use serde::Serialize;
use serde_json::ser::{Formatter, Serializer};
use std::collections::HashMap;
use std::io;
use std::time::Duration;

// Hard-coded timeouts: the upstream service is local, so 5s is enough to spot hangs.
// query has its own timeout so a hung backend cannot block query callers indefinitely.
const PREFETCH_TIMEOUT: Duration = Duration::from_secs(5);
const EVICT_TIMEOUT: Duration = Duration::from_secs(5);
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

struct CompactWithSpacesFormatter;

impl Formatter for CompactWithSpacesFormatter {
    fn begin_array_value<W>(&mut self, writer: &mut W, first: bool) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_key<W>(&mut self, writer: &mut W, first: bool) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_value<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        writer.write_all(b": ")
    }
}

fn to_json_with_spaces<T: Serialize>(value: &T) -> String {
    let mut buf = Vec::new();
    {
        let mut ser = Serializer::with_formatter(&mut buf, CompactWithSpacesFormatter);
        value.serialize(&mut ser).unwrap();
    }
    String::from_utf8(buf).unwrap()
}

pub struct LMCacheAscendBackend {
    base_url: String,
    client: reqwest::Client,
}

impl LMCacheAscendBackend {
    pub fn new(base_url: String) -> Self {
        let client = reqwest::Client::builder()
            // The upstream LMCache-Ascend service rejects requests with the
            // default reqwest UA ("reqwest/x.y.z"), so mimic curl/7.79.1 to
            // match what operators use for manual testing against the API.
            .user_agent("curl/7.79.1")
            .http1_title_case_headers()
            .build()
            .expect("failed to build reqwest client");
        Self { base_url, client }
    }

    #[allow(dead_code)]
    pub fn default_local() -> Self {
        Self::new("http://localhost:6999".to_string())
    }
}

#[async_trait]
impl KvCacheBackend for LMCacheAscendBackend {
    async fn prefetch(&self, hashes: Vec<String>, lookup_id: String) -> BackendResult {
        let payload = serde_json::json!({
            "chunk_hashes": hashes,
            "lookup_id": lookup_id,
        });
        let body = to_json_with_spaces(&payload);
        let count = hashes.len();
        match self
            .client
            .post(format!("{}/memory/prefetch", self.base_url))
            .header("Content-Type", "application/json")
            .header("Accept", "*/*")
            .timeout(PREFETCH_TIMEOUT)
            .body(body)
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
                    response = %body,
                    "LMCache prefetch response"
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
                    "LMCache prefetch request failed"
                );
                BackendResult {
                    status: 0,
                    responded: false,
                }
            }
        }
    }

    // Evict: POST {base_url}/memory/evict with chunk_hashes so LMCache releases those blocks from the NPU.
    async fn evict(&self, hashes: Vec<String>) -> BackendResult {
        let payload = serde_json::json!({
            "chunk_hashes": hashes,
        });
        let body = to_json_with_spaces(&payload);
        let count = hashes.len();
        match self
            .client
            .post(format!("{}/memory/evict", self.base_url))
            .header("Content-Type", "application/json")
            .header("Accept", "*/*")
            .timeout(EVICT_TIMEOUT)
            .body(body)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                tracing::info!(
                    status = status,
                    count = count,
                    response = %body,
                    "LMCache evict response"
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
                    "LMCache evict request failed"
                );
                BackendResult {
                    status: 0,
                    responded: false,
                }
            }
        }
    }

    // Query: POST {base_url}/memory/query with chunk_hashes to learn where each block lives in LMCache.
    // Returns locations: hash -> list of locations.
    async fn query(&self, hashes: Vec<String>) -> BackendQueryResult {
        let payload = serde_json::json!({
            "chunk_hashes": hashes,
        });
        let body = to_json_with_spaces(&payload);
        let count = hashes.len();
        match self
            .client
            .post(format!("{}/memory/query", self.base_url))
            .header("Content-Type", "application/json")
            .header("Accept", "*/*")
            .timeout(QUERY_TIMEOUT)
            .body(body)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body_text = resp.text().await.unwrap_or_default();
                tracing::info!(
                    status = status,
                    count = count,
                    response = %body_text,
                    "LMCache query response"
                );
                if let Ok(data) = serde_json::from_str::<HashMap<String, Vec<String>>>(&body_text) {
                    BackendQueryResult { locations: data }
                } else {
                    BackendQueryResult {
                        locations: HashMap::new(),
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, count = count, "LMCache query request failed");
                BackendQueryResult {
                    locations: HashMap::new(),
                }
            }
        }
    }
}

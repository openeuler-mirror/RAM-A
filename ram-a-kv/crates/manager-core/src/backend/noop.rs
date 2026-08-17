// NoopBackend: no-op backend for tests and development.
// Every operation returns success (HTTP 204) without sending real requests.
// Session tracking and refcount logic still run; only the actual cache ops are skipped.

use async_trait::async_trait;
use std::collections::HashMap;

use super::{BackendQueryResult, BackendResult, KvCacheBackend};

pub struct NoopBackend;

#[async_trait]
impl KvCacheBackend for NoopBackend {
    // Prefetch: return 204 (success, no content) without a real request.
    async fn prefetch(&self, _hashes: Vec<String>, _lookup_id: String) -> BackendResult {
        BackendResult {
            status: 204,
            responded: true,
        }
    }

    // Evict: return 204 without a real request.
    async fn evict(&self, _hashes: Vec<String>) -> BackendResult {
        BackendResult {
            status: 204,
            responded: true,
        }
    }

    // Query: return an empty result without a real request.
    async fn query(&self, _hashes: Vec<String>) -> BackendQueryResult {
        BackendQueryResult {
            locations: HashMap::new(),
        }
    }
}

// KV cache backend trait.
// Standard interface to the KV cache storage system (LMCache-Ascend).
// Implementations: NoopBackend (tests) and LMCacheAscendBackend (production).

use async_trait::async_trait;
use std::collections::HashMap;

// Result of a backend operation.
pub struct BackendResult {
    // HTTP response status code (200, 204, ...; 0 means the request was not sent / failed).
    pub status: u16,
    // Whether the backend responded (false = network error or timeout).
    pub responded: bool,
}

// Backend query result: storage locations per chunk hash.
pub struct BackendQueryResult {
    // hash -> location list (e.g. ["npu:0", "cpu:1"]).
    pub locations: HashMap<String, Vec<String>>,
}

// KV cache backend: implementations must provide prefetch, evict, and query.
#[async_trait]
pub trait KvCacheBackend: Send + Sync {
    // Prefetch: preload the given chunk hashes onto the accelerator; lookup_id identifies this request.
    async fn prefetch(&self, hashes: Vec<String>, lookup_id: String) -> BackendResult;
    // Evict: release the given chunk hashes from the accelerator.
    async fn evict(&self, hashes: Vec<String>) -> BackendResult;
    // Query: look up the storage locations of the given chunk hashes.
    async fn query(&self, hashes: Vec<String>) -> BackendQueryResult;
}

pub mod noop;

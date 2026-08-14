// manager-core: core logic library for the KV cache manager.
// Pure business logic: no HTTP service, SQLite persistence, or CLI.

pub mod backend;
pub mod config;
pub mod debug;
pub mod error;
pub mod manager;
pub mod map;

// Re-exported core types for the daemon layer.
pub use backend::{BackendQueryResult, BackendResult, KvCacheBackend};
pub use config::KvCacheConfig;
pub use error::{KvCacheError, ManagerResult};
pub use manager::KvCacheManager;
pub use map::KvCacheMap;

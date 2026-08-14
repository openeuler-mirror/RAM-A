// ram-a-kv daemon config: parsed from TOML, covers HTTP listen address, backend
// selection, debug settings, and the SQLite session-store path.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
    // Use the NoopBackend (no real LMCache traffic) when true.
    #[serde(default)]
    pub noop_backend: bool,
    #[serde(default = "default_lmcache_url")]
    pub lmcache_url: String,
    #[serde(default)]
    pub debug_enabled: bool,

    // Log a sessions + refcounts snapshot after every event when true.
    #[serde(default)]
    pub trace_events: bool,
    #[serde(default = "default_debug_dir")]
    pub debug_dir: String,
    #[serde(default = "default_session_store_path")]
    pub session_store_path: String,

    // Run prefetch during turn_start when true.
    #[serde(default = "default_turn_start_prefetch")]
    pub turn_start_prefetch: bool,

    // Optional bearer token for authenticating POST /event requests.
    // When non-empty, requests must carry `Authorization: Bearer <token>`.
    // Required when listen_addr is not a loopback address.
    #[serde(default)]
    pub auth_token: String,
}

fn default_listen_addr() -> String {
    // Default to loopback so the unauthenticated /event endpoint is not exposed
    // to other hosts. Operators who need cross-host access must set listen_addr
    // explicitly and configure `auth_token`.
    "127.0.0.1:6998".to_string()
}

fn default_lmcache_url() -> String {
    "http://localhost:6999".to_string()
}

fn default_debug_dir() -> String {
    "kvcache_debug".to_string()
}

fn default_session_store_path() -> String {
    "/var/lib/ram-a-kv/sessions.db".to_string()
}

fn default_turn_start_prefetch() -> bool {
    true
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listen_addr: default_listen_addr(),
            noop_backend: false,
            lmcache_url: default_lmcache_url(),
            debug_enabled: false,
            trace_events: false,
            debug_dir: default_debug_dir(),
            session_store_path: default_session_store_path(),
            turn_start_prefetch: default_turn_start_prefetch(),
            auth_token: String::new(),
        }
    }
}

impl DaemonConfig {
    pub fn from_toml_str(s: &str) -> Result<Self, manager_core::KvCacheError> {
        toml::from_str(s).map_err(
            |e: toml::de::Error| manager_core::KvCacheError::ConfigError {
                message: e.to_string(),
            },
        )
    }
}

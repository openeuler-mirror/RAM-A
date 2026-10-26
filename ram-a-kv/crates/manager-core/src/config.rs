// Copyright (c) 2026 Huawei Technologies Co.,Ltd. All rights reserved.
//
// RAM-A is licensed under Mulan PSL v2.
// You can use this software according to the terms and conditions of the Mulan
// PSL v2.
// You may obtain a copy of Mulan PSL v2 at:
//         http://license.coscl.org.cn/MulanPSL2
// THIS SOFTWARE IS PROVIDED ON AN "AS IS" BASIS, WITHOUT WARRANTIES OF ANY
// KIND, EITHER EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO
// NON-INFRINGEMENT, MERCHANTABILITY OR FIT FOR A PARTICULAR PURPOSE.
// See the Mulan PSL v2 for more details.

// KV cache coordinator configuration.
// Parseable from a TOML string or from xiaoO's LLM config section.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KvCacheConfig {
    // Enable KV caching (default: false).
    #[serde(default)]
    pub enabled: bool,

    // Enable debug file writing (default: false).
    #[serde(default)]
    pub debug_enabled: bool,

    // Log a sessions + refcounts snapshot after each event (default: false).
    #[serde(default)]
    pub trace_events: bool,

    // Debug file output directory (default: "kvcache_debug").
    #[serde(default = "default_debug_dir")]
    pub debug_dir: String,

    // LMCache backend service URL (default: "http://localhost:6999").
    #[serde(default = "default_backend_url")]
    pub backend_url: String,

    // Run prefetch at turn_start (default: true).
    #[serde(default = "default_turn_start_prefetch")]
    pub turn_start_prefetch: bool,
}

fn default_debug_dir() -> String {
    "kvcache_debug".to_string()
}

fn default_backend_url() -> String {
    "http://localhost:6999".to_string()
}

fn default_turn_start_prefetch() -> bool {
    true
}

impl Default for KvCacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            debug_enabled: false,
            trace_events: false,
            debug_dir: default_debug_dir(),
            backend_url: default_backend_url(),
            turn_start_prefetch: default_turn_start_prefetch(),
        }
    }
}

impl KvCacheConfig {
    // Extract KV-cache fields from xiaoO's LLM config section.
    // Recognizes the boolean keys `kvcache_enabled` and `kvcache_debug_enabled`.
    pub fn from_xiaoo_llm_section(llm: &toml::Value) -> Self {
        let enabled = llm
            .get("kvcache_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let debug_enabled = llm
            .get("kvcache_debug_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Self {
            enabled,
            debug_enabled,
            ..Self::default()
        }
    }

    // Parse config from a TOML string; returns ConfigError on failure.
    pub fn from_toml_str(s: &str) -> Result<Self, crate::KvCacheError> {
        toml::from_str(s).map_err(|e| crate::KvCacheError::ConfigError {
            message: e.to_string(),
        })
    }
}

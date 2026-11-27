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

//! Actrail-kv sink: forwards the actrail_capture field from turn_end payload
//! to an in-process actrail-kv receiver loaded via the Rust SDK.
//! Failures only warn; never affects the turn_end main flow.

use actrail_kv_sdk::{
    CaptureComparison, CaptureInput, Receiver, ReceiverFactory, SdkConfig, SdkOverrides,
};
use serde::Deserialize;
use serde_json::Value;
use std::sync::{Arc, Mutex};

pub struct ActrailSink {
    // Receiver is Send but not Sync (Instance guards exclusive access); wrap in
    // a Mutex so multiple async tasks can share it. The critical section is
    // short: submit() blocks until the store write completes, then the lock is
    // released. forward() runs submit() inside spawn_blocking so the async
    // runtime is never blocked.
    receiver: Arc<Mutex<Receiver>>,
    model_deployment_key: Option<String>,
    kv_namespace: Option<String>,
}

impl ActrailSink {
    /// Create a sink when `actrail_sdk_lib` is configured; returns `Ok(None)`
    /// when actrail integration is disabled (no `actrail_sdk_lib`). Returns
    /// `Err` on library load / ABI failure so the daemon can fail-fast.
    pub fn new(config: &crate::daemon_config::DaemonConfig) -> Result<Option<Self>, String> {
        let Some(lib) = config.actrail_sdk_lib.as_deref() else {
            return Ok(None);
        };
        if lib.trim().is_empty() {
            return Ok(None);
        }
        // base_dir: directory of the daemon config file, falling back to the
        // actrail default (~/.actrail/actrail-kv/). Explicit exports from the
        // SDK resolve relative paths against this base.
        let base_dir = config_base_dir()?;
        let receiver = ReceiverFactory::create(SdkConfig {
            library_path: lib.into(),
            base_dir,
            config_toml: None,
            overrides: SdkOverrides::default(),
        })
        .map_err(|e| format!("failed to load actrail receiver SDK from {lib}: {e}"))?;
        Ok(Some(Self {
            receiver: Arc::new(Mutex::new(receiver)),
            model_deployment_key: config.actrail_model_deployment_key.clone(),
            kv_namespace: config.actrail_kv_namespace.clone(),
        }))
    }

    /// Fire-and-forget forward of a captured turn_end payload. Returns
    /// immediately; any SDK/storage failure is logged as a warning.
    pub async fn forward(&self, capture: ActrailCapture, session_id: &str) {
        let receiver = self.receiver.clone();
        let model_deployment_key = self.model_deployment_key.clone();
        let kv_namespace = self.kv_namespace.clone();
        let input = CaptureInput {
            payload: capture.payload,
            source: Some("ram-a-kv-relay".into()),
            session_key: Some(session_id.to_owned()),
            comparison: CaptureComparison {
                endpoint_key: capture.endpoint_key,
                agent_key: capture.agent_key,
                model_deployment_key,
                kv_namespace,
            },
        };
        // submit() is a blocking call (it drives an in-library tokio runtime
        // and waits for the store write). Run it on the blocking pool so the
        // async runtime is not stalled and turn_end returns immediately.
        tokio::task::spawn_blocking(move || {
            match receiver.lock().expect("actrail receiver mutex poisoned").submit(&input) {
                Ok(receipt) => {
                    tracing::info!(captured_at = %receipt.captured_at, "actrail sdk submit ok");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "actrail sdk submit failed");
                }
            }
        });
    }
}

/// Resolve the base_dir for SDK relative paths. Uses the directory containing
/// the daemon config file when available, otherwise the actrail default.
fn config_base_dir() -> Result<std::path::PathBuf, String> {
    use std::path::PathBuf;
    let path = std::env::var("RAM_A_KV_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/etc/ram-a-kv/config.toml"));
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            return Ok(parent.to_path_buf());
        }
    }
    // Fallback: actrail default settings directory.
    home::actrail_default_base_dir()
}

mod home {
    use std::path::PathBuf;
    pub fn actrail_default_base_dir() -> Result<PathBuf, String> {
        dirs::home_dir()
            .map(|h| h.join(".actrail").join("actrail-kv"))
            .ok_or_else(|| "could not locate home directory for actrail base_dir".to_owned())
    }
}

/// Deserialization struct for the actrail_capture field in turn_end payload.
#[derive(Deserialize, Clone)]
pub struct ActrailCapture {
    pub endpoint_key: String,
    #[serde(default)]
    pub agent_key: Option<String>,
    pub payload: Value,
}

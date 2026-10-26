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

//! Actrail-kv relay: forwards the actrail_capture field from turn_end payload to actrail-kv-receiver.
//! Failures only warn; never affects the turn_end main flow.

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone)]
pub struct ActrailRelay {
    http: reqwest::Client,
    receiver_url: String,
    model_deployment_key: Option<String>,
    kv_namespace: Option<String>,
}

impl ActrailRelay {
    pub fn new(config: &crate::daemon_config::DaemonConfig) -> Option<Self> {
        let url = config.actrail_receiver_url.clone()?;
        if url.trim().is_empty() {
            return None;
        }
        Some(Self {
            http: reqwest::Client::new(),
            receiver_url: url,
            model_deployment_key: config.actrail_model_deployment_key.clone(),
            kv_namespace: config.actrail_kv_namespace.clone(),
        })
    }

    pub async fn forward(&self, capture: ActrailCapture, session_id: &str) {
        let url = format!("{}/requests", self.receiver_url);
        let mut req = self.http
            .post(&url)
            .header("X-Actrail-Endpoint-Key", &capture.endpoint_key)
            .header("X-Actrail-Source", "ram-a-kv-relay")
            .header("X-Actrail-Session-Key", session_id)
            .json(&capture.payload);
        if let Some(agent_key) = &capture.agent_key {
            req = req.header("X-Actrail-Agent-Key", agent_key);
        }
        if let Some(deployment) = &self.model_deployment_key {
            req = req.header("X-Actrail-Model-Deployment-Key", deployment);
        }
        if let Some(ns) = &self.kv_namespace {
            req = req.header("X-Actrail-KV-Namespace", ns);
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!(status = %resp.status(), "actrail forward ok");
            }
            Ok(resp) => {
                tracing::warn!(status = %resp.status(), "actrail receiver returned non-2xx");
            }
            Err(e) => {
                tracing::warn!(error = %e, "actrail forward failed");
            }
        }
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

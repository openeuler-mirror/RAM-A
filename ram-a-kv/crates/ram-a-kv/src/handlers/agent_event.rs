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

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::event_registry::{AppContext, EventHandler, EventResult, EventSpec};

// Acknowledges the framework-neutral agent-turn boundary that DeepAgents
// middleware reports in its `before_agent` hook. The daemon does not act on it
// today; the handler exists so the notification is accepted instead of being
// reported as an unknown/degraded event.
pub struct AgentEventHandler;

#[async_trait]
impl EventHandler for AgentEventHandler {
    fn spec(&self) -> EventSpec {
        EventSpec {
            description: "Acknowledge an agent turn boundary reported by the framework middleware"
                .into(),
            required: vec!["session_id".into()],
            optional: vec![
                "event_name".into(),
                "event_action".into(),
                "source".into(),
                "agent_turn_id".into(),
                "event_id".into(),
                "payload".into(),
                "detail_level".into(),
                "schema_version".into(),
            ],
        }
    }

    async fn handle(&self, payload: &Value, _ctx: &AppContext) -> EventResult {
        let session_id = payload["session_id"].as_str().unwrap_or("");
        if session_id.is_empty() {
            return EventResult::Err("session_id must not be empty".into());
        }
        EventResult::Ok(json!({ "accepted": true }))
    }
}

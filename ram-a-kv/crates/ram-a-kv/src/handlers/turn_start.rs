use async_trait::async_trait;
use manager_core::manager::OperationOutcome;
use serde_json::{json, Value};

use crate::event_registry::{AppContext, EventHandler, EventResult, EventSpec};

pub struct TurnStartHandler;

#[async_trait]
impl EventHandler for TurnStartHandler {
    fn spec(&self) -> EventSpec {
        EventSpec {
            description: "Pre-inference prefetch: daemon reads chunk_hashes from the session map and prefetches".into(),
            required: vec!["session_id".into()],
            optional: vec![],
        }
    }

    async fn handle(&self, payload: &Value, ctx: &AppContext) -> EventResult {
        let session_id = payload["session_id"].as_str().unwrap_or("").to_string();
        if session_id.is_empty() {
            return EventResult::Err("session_id must not be empty".into());
        }
        let outcome: OperationOutcome = match ctx.manager.on_turn_start(&session_id).await {
            Ok(o) => o,
            Err(e) => return EventResult::Err(e.to_string()),
        };
        EventResult::Ok(json!({
            "prefetch_sent": outcome.prefetch_sent,
            "prefetch_count": outcome.prefetch_count,
            "backend_degraded": outcome.backend_degraded,
        }))
    }
}

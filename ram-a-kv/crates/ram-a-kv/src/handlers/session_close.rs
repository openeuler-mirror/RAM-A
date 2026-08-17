use async_trait::async_trait;
use manager_core::manager::OperationOutcome;
use serde_json::{json, Value};

use crate::event_registry::{AppContext, EventHandler, EventResult, EventSpec};

pub struct SessionCloseHandler;

#[async_trait]
impl EventHandler for SessionCloseHandler {
    fn spec(&self) -> EventSpec {
        EventSpec {
            description: "Close session: release references and evict refcount==0 chunks; pinned sessions keep their SQLite row".into(),
            required: vec!["session_id".into()],
            optional: vec![],
        }
    }

    async fn handle(&self, payload: &Value, ctx: &AppContext) -> EventResult {
        let session_id = payload["session_id"].as_str().unwrap_or("").to_string();
        if session_id.is_empty() {
            return EventResult::Err("session_id must not be empty".into());
        }
        // Cleanup runs regardless of pin status; the pin only guards the SQLite
        // row (see api.rs).
        let pinned = ctx.session_store.is_pinned(&session_id);
        let outcome: OperationOutcome = match ctx.manager.on_session_close(&session_id).await {
            Ok(o) => o,
            Err(e) => return EventResult::Err(e.to_string()),
        };
        EventResult::Ok(json!({
            "closed": true,
            "pinned": pinned,
            "evicted_count": outcome.evicted_count,
            "backend_degraded": outcome.backend_degraded,
        }))
    }
}

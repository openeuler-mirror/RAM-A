use async_trait::async_trait;
use manager_core::manager::OperationOutcome;
use serde_json::{json, Value};

use crate::event_registry::{AppContext, EventHandler, EventResult, EventSpec};

pub struct SessionSuspendHandler;

#[async_trait]
impl EventHandler for SessionSuspendHandler {
    fn spec(&self) -> EventSpec {
        EventSpec {
            description: "Suspend session: pin the SQLite row for snapshot restore, then release references and evict".into(),
            required: vec!["session_id".into()],
            optional: vec![],
        }
    }

    async fn handle(&self, payload: &Value, ctx: &AppContext) -> EventResult {
        let session_id = payload["session_id"].as_str().unwrap_or("").to_string();
        if session_id.is_empty() {
            return EventResult::Err("session_id must not be empty".into());
        }
        // Pin first so the SQLite row survives a later session_close.
        ctx.session_store.pin_session(&session_id);
        // Then release in-memory references and evict refcount==0 chunks;
        // the next snapshot load triggers a fresh prefill in vLLM.
        let outcome: OperationOutcome = match ctx.manager.on_session_close(&session_id).await {
            Ok(o) => o,
            Err(e) => return EventResult::Err(e.to_string()),
        };
        EventResult::Ok(json!({
            "pinned": true,
            "suspended": true,
            "evicted_count": outcome.evicted_count,
            "backend_degraded": outcome.backend_degraded,
        }))
    }
}

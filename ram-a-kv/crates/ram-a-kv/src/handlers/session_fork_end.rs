use async_trait::async_trait;
use manager_core::manager::OperationOutcome;
use serde_json::{json, Value};

use crate::event_registry::{AppContext, EventHandler, EventResult, EventSpec};

pub struct SessionForkEndHandler;

#[async_trait]
impl EventHandler for SessionForkEndHandler {
    fn spec(&self) -> EventSpec {
        EventSpec {
            description: "Fork end: decrement refcount on the session's chunk_hashes, evict blocks with refcount = 0".into(),
            required: vec!["session_id".into()],
            // fork_id disambiguates which fork to end when multiple forks are
            // active for one session. When omitted, the most recent fork is used.
            optional: vec!["fork_id".into()],
        }
    }

    async fn handle(&self, payload: &Value, ctx: &AppContext) -> EventResult {
        let session_id = payload["session_id"].as_str().unwrap_or("").to_string();
        if session_id.is_empty() {
            return EventResult::Err("session_id must not be empty".into());
        }
        // fork_id is optional: callers that received a fork_id from session_fork
        // should pass it back; older callers omit it and we use the latest fork.
        let fork_id = payload["fork_id"].as_u64();
        let outcome: OperationOutcome =
            match ctx.manager.on_session_fork_end(&session_id, fork_id).await {
                Ok(o) => o,
                Err(e) => return EventResult::Err(e.to_string()),
            };
        EventResult::Ok(json!({
            "fork_end": true,
            "evicted_count": outcome.evicted_count,
            "backend_degraded": outcome.backend_degraded,
            "matched": outcome.evicted_count > 0 || !outcome.backend_degraded,
        }))
    }
}

use async_trait::async_trait;
use manager_core::manager::ForkOutcome;
use serde_json::{json, Value};

use crate::event_registry::{AppContext, EventHandler, EventResult, EventSpec};

pub struct SessionForkHandler;

#[async_trait]
impl EventHandler for SessionForkHandler {
    fn spec(&self) -> EventSpec {
        EventSpec {
            description: "Fork session: increment refcount on the session's chunk_hashes to prevent parent turn_end from evicting".into(),
            required: vec!["session_id".into()],
            optional: vec![],
        }
    }

    async fn handle(&self, payload: &Value, ctx: &AppContext) -> EventResult {
        let session_id = payload["session_id"].as_str().unwrap_or("").to_string();
        if session_id.is_empty() {
            return EventResult::Err("session_id must not be empty".into());
        }
        let outcome: ForkOutcome = match ctx.manager.on_session_fork(&session_id).await {
            Ok(o) => o,
            Err(e) => return EventResult::Err(e.to_string()),
        };
        EventResult::Ok(json!({
            "forked": true,
            "fork_id": outcome.fork_id,
        }))
    }
}

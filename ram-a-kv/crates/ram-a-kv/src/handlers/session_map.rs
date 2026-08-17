use async_trait::async_trait;
use serde_json::{json, Value};

use crate::event_registry::{AppContext, EventHandler, EventResult, EventSpec};

pub struct SessionMapHandler;

#[async_trait]
impl EventHandler for SessionMapHandler {
    fn spec(&self) -> EventSpec {
        EventSpec {
            description: "Get session cache state".into(),
            required: vec!["session_id".into()],
            optional: vec![],
        }
    }

    async fn handle(&self, payload: &Value, ctx: &AppContext) -> EventResult {
        let session_id = payload["session_id"].as_str().unwrap_or("").to_string();
        if session_id.is_empty() {
            return EventResult::Err("session_id must not be empty".into());
        }
        match ctx.manager.session_map(&session_id).await {
            Some(map) => EventResult::Ok(json!({
                "session_id": session_id,
                "chunk_hashes": map,
            })),
            None => EventResult::Err(format!("session '{}' not found", session_id)),
        }
    }
}

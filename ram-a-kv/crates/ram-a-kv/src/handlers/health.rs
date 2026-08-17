use async_trait::async_trait;
use serde_json::{json, Value};

use crate::event_registry::{AppContext, EventHandler, EventResult, EventSpec};

pub struct HealthHandler;

#[async_trait]
impl EventHandler for HealthHandler {
    fn spec(&self) -> EventSpec {
        EventSpec {
            description: "Health check".into(),
            required: vec![],
            optional: vec![],
        }
    }

    async fn handle(&self, _payload: &Value, ctx: &AppContext) -> EventResult {
        let sessions_count = ctx.manager.sessions_count().await;
        EventResult::Ok(json!({
            "status": "running",
            "sessions_count": sessions_count,
        }))
    }
}

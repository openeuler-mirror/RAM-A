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
        if ctx.turn_start_prefetch_async {
            // Fire-and-forget: acknowledge immediately so the router fan-out runs
            // concurrently with the caller's next work (the model call) instead of
            // blocking it. The prefetch result is only logged, not returned.
            let manager = ctx.manager.clone();
            let sid = session_id.clone();
            tokio::spawn(async move {
                match manager.on_turn_start(&sid).await {
                    Ok(o) => tracing::info!(
                        session_id = %sid,
                        prefetch_count = o.prefetch_count,
                        prefetch_sent = o.prefetch_sent,
                        "async turn_start prefetch dispatched"
                    ),
                    Err(e) => tracing::warn!(
                        session_id = %sid,
                        error = %e,
                        "async turn_start prefetch failed"
                    ),
                }
            });
            return EventResult::Ok(json!({
                "prefetch_sent": true,
                "prefetch_count": null,
                "backend_degraded": false,
                "async": true,
            }));
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

use async_trait::async_trait;
use manager_core::manager::OperationOutcome;
use serde_json::{json, Value};

use crate::event_registry::{AppContext, EventHandler, EventResult, EventSpec};

pub struct SnapshotRestoreHandler;

#[async_trait]
impl EventHandler for SnapshotRestoreHandler {
    fn spec(&self) -> EventSpec {
        EventSpec {
            description: "Restore session snapshot: load chunk_hashes from memory or persistent storage, register references + prefetch".into(),
            required: vec!["session_id".into()],
            optional: vec![],
        }
    }

    async fn handle(&self, payload: &Value, ctx: &AppContext) -> EventResult {
        let session_id = payload["session_id"].as_str().unwrap_or("").to_string();
        if session_id.is_empty() {
            return EventResult::Err("session_id must not be empty".into());
        }

        let chunk_hashes = match ctx.manager.session_map(&session_id).await {
            Some(map) if !map.is_empty() => map.chunk_hashes(),
            _ => {
                // Fall back to SQLite when the session is not in memory
                // (e.g., the daemon restarted and the session was suspended
                // before the restart).
                let Some((map, _turn_count)) = ctx.session_store.load(&session_id) else {
                    return EventResult::Err(format!(
                        "session '{}' not found in memory or store",
                        session_id
                    ));
                };
                if map.is_empty() {
                    return EventResult::Err(format!(
                        "session '{}' has no chunk_hashes to restore",
                        session_id
                    ));
                }
                map.chunk_hashes()
            }
        };

        let chunk_count = chunk_hashes.len();
        let outcome: OperationOutcome = match ctx
            .manager
            .on_snapshot_restore(&session_id, chunk_hashes)
            .await
        {
            Ok(o) => o,
            Err(e) => return EventResult::Err(e.to_string()),
        };

        // Fallback pin: covers the case where the daemon was offline while the
        // snapshot was saved and the suspend-time pin never landed.
        ctx.session_store.pin_session(&session_id);

        EventResult::Ok(json!({
            "prefetch_sent": outcome.prefetch_sent,
            "prefetch_count": chunk_count,
            "backend_degraded": outcome.backend_degraded,
            "evicted_count": outcome.evicted_count,
            "pinned": true,
        }))
    }
}

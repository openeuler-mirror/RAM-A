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
            optional: vec!["source_session_id".into()],
        }
    }

    async fn handle(&self, payload: &Value, ctx: &AppContext) -> EventResult {
        let target_session_id = payload["session_id"].as_str().unwrap_or("").to_string();
        if target_session_id.is_empty() {
            return EventResult::Err("session_id must not be empty".into());
        }
        // When a new session is resumed from a previous one, the caller must pass
        // source_session_id so we read the map from the previous session (whose
        // hashes survive in memory or SQLite). Defaulting to the target session
        // would query an empty new session and return "not found".
        let source_session_id = payload["source_session_id"]
            .as_str()
            .unwrap_or(&target_session_id)
            .to_string();

        let chunk_hashes = match ctx.manager.session_map(&source_session_id).await {
            Some(map) if !map.is_empty() => map.chunk_hashes(),
            _ => {
                // Fall back to SQLite when the source session is not in memory
                // (e.g., the daemon restarted and the session was suspended
                // before the restart).
                let Some((map, _turn_count)) = ctx.session_store.load(&source_session_id) else {
                    return EventResult::Err(format!(
                        "source session '{}' not found in memory or store",
                        source_session_id
                    ));
                };
                if map.is_empty() {
                    return EventResult::Err(format!(
                        "source session '{}' has no chunk_hashes to restore",
                        source_session_id
                    ));
                }
                map.chunk_hashes()
            }
        };

        let chunk_count = chunk_hashes.len();
        let outcome: OperationOutcome = match ctx
            .manager
            .on_snapshot_restore_from(&source_session_id, &target_session_id, chunk_hashes)
            .await
        {
            Ok(o) => o,
            Err(e) => return EventResult::Err(e.to_string()),
        };

        // Fallback pin: covers the case where the daemon was offline while the
        // snapshot was saved and the suspend-time pin never landed.
        ctx.session_store.pin_session(&source_session_id);

        EventResult::Ok(json!({
            "prefetch_sent": outcome.prefetch_sent,
            "prefetch_count": chunk_count,
            "backend_degraded": outcome.backend_degraded,
            "evicted_count": outcome.evicted_count,
            "pinned": true,
            "source_session_id": source_session_id,
            "target_session_id": target_session_id,
        }))
    }
}

use async_trait::async_trait;
use manager_core::debug::{DebugContext, TimingInfo};
use manager_core::manager::OperationOutcome;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::event_registry::{AppContext, EventHandler, EventResult, EventSpec};
use crate::handlers::acttrail_relay::ActtrailCapture;

// Hard-coded input limits for turn_end chunk_hashes.
// Each chunk hash is at most 100 bytes; a single request carries at most 1000
// chunks. Oversized payloads are rejected before reaching the manager so a
// hostile or buggy client cannot blow up memory or stall eviction.
const MAX_CHUNK_HASH_LEN: usize = 100;
const MAX_CHUNK_COUNT: usize = 1000;

pub struct TurnEndHandler;

// Strict typed payload so missing or invalid `chunk_hashes` is rejected
// instead of being silently interpreted as "clear the session".
#[derive(Deserialize)]
struct KvTransferParamsPayload {
    // Required: must be present and an array of strings. Explicit [] is allowed
    // (clears the session), but null/missing is an error.
    chunk_hashes: Vec<String>,
}

#[derive(Deserialize)]
struct TurnEndPayload {
    session_id: String,
    // Optional: absent when the caller has no KV transfer data (e.g. direct
    // model connection). When present, chunk_hashes drive normal eviction.
    #[serde(default)]
    kv_transfer_params: Option<KvTransferParamsPayload>,
    #[serde(default)]
    debug_context: Option<Value>,
    #[serde(default)]
    acttrail_capture: Option<ActtrailCapture>,
}

#[async_trait]
impl EventHandler for TurnEndHandler {
    fn spec(&self) -> EventSpec {
        EventSpec {
            description: "Post-inference safe eviction + update: decrement refcount, evict only when refcount == 0".into(),
            required: vec!["session_id".into()],
            optional: vec!["kv_transfer_params".into(), "debug_context".into(), "acttrail_capture".into()],
        }
    }

    async fn handle(&self, payload: &Value, ctx: &AppContext) -> EventResult {
        // Deserialize into a strict structure so a missing chunk_hashes field
        // is rejected (vs. silently treated as an empty array that clears the
        // session).
        let parsed: TurnEndPayload = match serde_json::from_value(payload.clone()) {
            Ok(p) => p,
            Err(e) => return EventResult::Err(format!("invalid turn_end payload: {e}")),
        };
        if parsed.session_id.is_empty() {
            return EventResult::Err("session_id must not be empty".into());
        }

        let chunk_hashes = parsed
            .kv_transfer_params
            .as_ref()
            .map(|kvt| kvt.chunk_hashes.clone())
            .unwrap_or_default();
        if chunk_hashes.len() > MAX_CHUNK_COUNT {
            return EventResult::Err(format!(
                "chunk_hashes count {} exceeds limit of {}",
                chunk_hashes.len(),
                MAX_CHUNK_COUNT
            ));
        }
        if let Some((i, h)) = chunk_hashes
            .iter()
            .enumerate()
            .find(|(_, h)| h.len() > MAX_CHUNK_HASH_LEN)
        {
            return EventResult::Err(format!(
                "chunk_hashes[{i}] length {} exceeds limit of {} bytes",
                h.len(),
                MAX_CHUNK_HASH_LEN
            ));
        }
        let chunk_count = chunk_hashes.len();

        let debug_context = parsed.debug_context.as_ref().and_then(|dc| {
            let messages = dc.get("messages").and_then(|m| m.as_array()).cloned();
            let timing = dc.get("timing").map(|t| TimingInfo {
                ttft_ms: t.get("ttft_ms").and_then(|v| v.as_u64()),
                total_time_ms: t.get("total_time_ms").and_then(|v| v.as_u64()),
                tpot_ms: t.get("tpot_ms").and_then(|v| v.as_u64()),
            });
            if messages.is_some() || timing.is_some() {
                Some(DebugContext {
                    messages: messages.unwrap_or_default(),
                    timing: timing.unwrap_or(TimingInfo {
                        ttft_ms: None,
                        total_time_ms: None,
                        tpot_ms: None,
                    }),
                })
            } else {
                None
            }
        });

        let outcome: OperationOutcome = match ctx
            .manager
            .on_turn_end(&parsed.session_id, chunk_hashes, debug_context)
            .await
        {
            Ok(o) => o,
            Err(e) => return EventResult::Err(e.to_string()),
        };

        if let (Some(relay), Some(capture)) = (&ctx.acttrail_relay, parsed.acttrail_capture.clone()) {
            let session_id = parsed.session_id.clone();
            let relay = relay.clone();
            tokio::spawn(async move {
                relay.forward(capture, &session_id).await;
            });
        }

        EventResult::Ok(json!({
            "evicted_count": outcome.evicted_count,
            "map_updated": true,
            "debug_written": outcome.debug_written,
            "backend_degraded": outcome.backend_degraded,
            "chunk_count": chunk_count,
        }))
    }
}

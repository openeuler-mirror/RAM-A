use async_trait::async_trait;
use manager_core::debug::{DebugContext, TimingInfo};
use manager_core::manager::OperationOutcome;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::event_registry::{AppContext, EventHandler, EventResult, EventSpec};
use crate::handlers::actrail_relay::ActrailCapture;

// Hard-coded input limits for turn_end chunk_hashes.
// Each chunk hash is at most 100 bytes; a single request carries at most 4096
// chunks (was 1000; raised so that very long sessions can map their KV -- the
// engine's block size is 128 tokens, so a ~256k-token context needs ~2000
// chunks). Oversized payloads are still rejected before reaching the manager so
// a hostile or buggy client cannot blow up memory or stall eviction.
const MAX_CHUNK_HASH_LEN: usize = 100;
const MAX_CHUNK_COUNT: usize = 4096;

pub struct TurnEndHandler;

// `kv_transfer_params` is optional so a caller with no KV data can omit it;
// when present, `chunk_hashes` must be an array of strings.
//
// Semantics: an absent `kv_transfer_params` (or a response that carried no
// cache identifiers) is a no-op for the session map, while an explicit `[]`
// clears the session. This keeps a single turn that yields no `ucm_block_ids`
// from silently wiping a session's prefetch map.
#[derive(Deserialize)]
struct KvTransferParamsPayload {
    // Required when `kv_transfer_params` is present: an array of strings.
    // Explicit [] is allowed (clears the session); null is an error.
    chunk_hashes: Vec<String>,
}

#[derive(Deserialize)]
struct TurnEndPayload {
    session_id: String,
    // Optional: absent when the caller has no KV transfer data (e.g. a response
    // with no ucm_block_ids, or a direct model connection). Absent is a no-op
    // for the session map; only an explicit `{"chunk_hashes": []}` clears it.
    #[serde(default)]
    kv_transfer_params: Option<KvTransferParamsPayload>,
    #[serde(default)]
    debug_context: Option<Value>,
    #[serde(default)]
    actrail_capture: Option<ActrailCapture>,
}

#[async_trait]
impl EventHandler for TurnEndHandler {
    fn spec(&self) -> EventSpec {
        EventSpec {
            description: "Post-inference safe eviction + update: decrement refcount, evict only when refcount == 0".into(),
            required: vec!["session_id".into()],
            optional: vec!["kv_transfer_params".into(), "debug_context".into(), "actrail_capture".into()],
        }
    }

    async fn handle(&self, payload: &Value, ctx: &AppContext) -> EventResult {
        // Deserialize into a strict structure so an invalid `chunk_hashes`
        // (e.g. null or a non-array) is rejected rather than silently treated
        // as an empty array that clears the session.
        let parsed: TurnEndPayload = match serde_json::from_value(payload.clone()) {
            Ok(p) => p,
            Err(e) => return EventResult::Err(format!("invalid turn_end payload: {e}")),
        };
        if parsed.session_id.is_empty() {
            return EventResult::Err("session_id must not be empty".into());
        }

        // `None` when the field is absent: the caller had no KV data this turn,
        // so the session map must be left untouched. `Some(vec![])` is an
        // explicit clear.
        let chunk_hashes: Option<Vec<String>> = parsed
            .kv_transfer_params
            .as_ref()
            .map(|kvt| kvt.chunk_hashes.clone());
        if let Some(hashes) = &chunk_hashes {
            if hashes.len() > MAX_CHUNK_COUNT {
                return EventResult::Err(format!(
                    "chunk_hashes count {} exceeds limit of {}",
                    hashes.len(),
                    MAX_CHUNK_COUNT
                ));
            }
            if let Some((i, h)) = hashes
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
        }
        let map_updated = chunk_hashes.is_some();
        let chunk_count = chunk_hashes.as_ref().map_or(0, |h| h.len());

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

        if let (Some(relay), Some(capture)) = (&ctx.actrail_relay, parsed.actrail_capture.clone()) {
            let session_id = parsed.session_id.clone();
            let relay = relay.clone();
            tokio::spawn(async move {
                relay.forward(capture, &session_id).await;
            });
        }

        EventResult::Ok(json!({
            "evicted_count": outcome.evicted_count,
            "map_updated": map_updated,
            "debug_written": outcome.debug_written,
            "backend_degraded": outcome.backend_degraded,
            "chunk_count": chunk_count,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::turn_start::TurnStartHandler;
    use crate::session_store::SqliteSessionStore;
    use async_trait::async_trait;
    use manager_core::{BackendQueryResult, BackendResult, KvCacheBackend, KvCacheConfig, KvCacheManager};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    // Records every prefetch the manager sends, so a test can assert that a
    // later turn_start still prefetches after a no-op turn_end.
    #[derive(Default)]
    struct RecordingBackend {
        prefetches: Mutex<Vec<Vec<String>>>,
    }

    #[async_trait]
    impl KvCacheBackend for RecordingBackend {
        async fn prefetch(
            &self,
            hashes: Vec<String>,
            _lookup_id: String,
            _session_id: String,
        ) -> BackendResult {
            self.prefetches.lock().unwrap().push(hashes);
            BackendResult {
                status: 200,
                responded: true,
            }
        }
        async fn evict(&self, _hashes: Vec<String>) -> BackendResult {
            BackendResult {
                status: 200,
                responded: true,
            }
        }
        async fn query(&self, _hashes: Vec<String>) -> BackendQueryResult {
            BackendQueryResult {
                locations: HashMap::new(),
            }
        }
    }

    async fn make_ctx(backend: Arc<RecordingBackend>) -> AppContext {
        let config = KvCacheConfig {
            enabled: true,
            turn_start_prefetch: true,
            ..Default::default()
        };
        AppContext {
            manager: Arc::new(KvCacheManager::new(config, backend)),
            session_store: Arc::new(SqliteSessionStore::new(":memory:").unwrap()),
            actrail_relay: None,
            turn_start_prefetch_async: false,
        }
    }

    fn ok(result: EventResult) -> Value {
        match result {
            EventResult::Ok(v) => v,
            EventResult::Err(e) => panic!("expected Ok, got Err: {e}"),
        }
    }

    // End-to-end over the real handlers: a session maps ["a","b"], then a middle
    // turn_end arrives *without* `kv_transfer_params` (the deepagents middleware
    // omits it when a response yields no ucm_block_ids). The map must survive and
    // the very next turn_start must still prefetch, then an explicit [] clears it.
    #[tokio::test]
    async fn middle_turn_without_kv_params_keeps_map_and_next_prefetch_works() {
        let backend = Arc::new(RecordingBackend::default());
        let ctx = make_ctx(backend.clone()).await;
        let start = TurnStartHandler;
        let end = TurnEndHandler;
        let sid = "deepagents:regression";

        // No session yet -> nothing to prefetch.
        let r = ok(start.handle(&json!({ "session_id": sid }), &ctx).await);
        assert_eq!(r["prefetch_sent"].as_bool(), Some(false));
        assert_eq!(r["prefetch_count"].as_u64(), Some(0));

        // Turn 1: real KV data arrives and builds the map.
        let r = ok(
            end.handle(
                &json!({ "session_id": sid, "kv_transfer_params": { "chunk_hashes": ["a", "b"] } }),
                &ctx,
            )
            .await,
        );
        assert_eq!(r["map_updated"].as_bool(), Some(true));
        assert_eq!(r["chunk_count"].as_u64(), Some(2));

        // Turn 2 start: the map is prefetched.
        let r = ok(start.handle(&json!({ "session_id": sid }), &ctx).await);
        assert_eq!(r["prefetch_sent"].as_bool(), Some(true));
        assert_eq!(r["prefetch_count"].as_u64(), Some(2));

        // Turn 2 end: no kv_transfer_params at all (middle request, no ucm_block_ids).
        let r = ok(end.handle(&json!({ "session_id": sid }), &ctx).await);
        assert_eq!(r["map_updated"].as_bool(), Some(false));
        assert_eq!(r["chunk_count"].as_u64(), Some(0));
        assert_eq!(r["evicted_count"].as_u64(), Some(0));

        // The map must be intact...
        let map = ctx.manager.session_map(sid).await.expect("session must exist");
        assert_eq!(map.chunk_hashes(), vec!["a".to_string(), "b".to_string()]);

        // ...so turn 3 start still prefetches the same hashes.
        let r = ok(start.handle(&json!({ "session_id": sid }), &ctx).await);
        assert_eq!(r["prefetch_sent"].as_bool(), Some(true));
        assert_eq!(r["prefetch_count"].as_u64(), Some(2));

        let calls = backend.prefetches.lock().unwrap();
        assert_eq!(calls.len(), 2, "both turn_starts must have prefetched");
        assert_eq!(calls[0], vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            calls[1],
            vec!["a".to_string(), "b".to_string()],
            "no-op turn_end must not change what is prefetched"
        );
    }

    // An explicit empty array is the only way to clear the session.
    #[tokio::test]
    async fn explicit_empty_chunk_hashes_clears_and_stops_prefetch() {
        let backend = Arc::new(RecordingBackend::default());
        let ctx = make_ctx(backend.clone()).await;
        let start = TurnStartHandler;
        let end = TurnEndHandler;
        let sid = "deepagents:clear";

        ok(end
            .handle(
                &json!({ "session_id": sid, "kv_transfer_params": { "chunk_hashes": ["a"] } }),
                &ctx,
            )
            .await);
        let r = ok(start.handle(&json!({ "session_id": sid }), &ctx).await);
        assert_eq!(r["prefetch_sent"].as_bool(), Some(true));

        // Explicit clear.
        let r = ok(
            end.handle(
                &json!({ "session_id": sid, "kv_transfer_params": { "chunk_hashes": [] } }),
                &ctx,
            )
            .await,
        );
        assert_eq!(r["map_updated"].as_bool(), Some(true));
        assert_eq!(r["chunk_count"].as_u64(), Some(0));
        assert_eq!(r["evicted_count"].as_u64(), Some(1));

        let map = ctx.manager.session_map(sid).await.expect("session must exist");
        assert!(map.chunk_hashes().is_empty());

        // Nothing left to prefetch.
        let r = ok(start.handle(&json!({ "session_id": sid }), &ctx).await);
        assert_eq!(r["prefetch_sent"].as_bool(), Some(false));
    }

    // A present-but-null chunk_hashes is still rejected (never silently clears).
    #[tokio::test]
    async fn null_chunk_hashes_is_rejected() {
        let backend = Arc::new(RecordingBackend::default());
        let ctx = make_ctx(backend).await;
        let end = TurnEndHandler;
        let sid = "deepagents:null";

        ok(end
            .handle(
                &json!({ "session_id": sid, "kv_transfer_params": { "chunk_hashes": ["a"] } }),
                &ctx,
            )
            .await);

        match end
            .handle(
                &json!({ "session_id": sid, "kv_transfer_params": { "chunk_hashes": null } }),
                &ctx,
            )
            .await
        {
            EventResult::Err(_) => {}
            EventResult::Ok(v) => panic!("null chunk_hashes must be rejected, got: {v}"),
        }

        // Map untouched by the rejected request.
        let map = ctx.manager.session_map(sid).await.expect("session must exist");
        assert_eq!(map.chunk_hashes(), vec!["a".to_string()]);
    }
}

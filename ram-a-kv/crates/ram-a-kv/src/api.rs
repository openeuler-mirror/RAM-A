// HTTP API: all client requests hit POST /event and are dispatched by the JSON "type" field.

use axum::extract::State;
use axum::Json;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

use crate::event_registry::{AppContext, EventRegistry, EventResult};

// Unified response envelope returned by POST /event.
#[derive(Serialize)]
pub struct EventResponse {
    ok: bool,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    type_: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl EventResponse {
    pub fn ok(type_: &str, data: Value) -> Self {
        Self {
            ok: true,
            type_: Some(type_.to_string()),
            data: Some(data),
            error: None,
        }
    }

    pub fn error(type_: Option<&str>, message: &str) -> Self {
        Self {
            ok: false,
            type_: type_.map(String::from),
            data: None,
            error: Some(message.to_string()),
        }
    }
}

// Application state shared with every request via Axum State.
pub struct AppState {
    pub manager: Arc<manager_core::KvCacheManager>,
    pub registry: Arc<EventRegistry>,
    pub session_store: Arc<crate::session_store::SqliteSessionStore>,
    pub context: Arc<AppContext>,
    pub auth_token: String,
}

// Handle a POST /event request. Body is JSON and must contain a "type" field.
pub async fn handle_event(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> Json<EventResponse> {
    let type_str = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let session_id = payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if type_str.is_empty() {
        tracing::warn!("event rejected: missing or empty 'type' field");
        return Json(EventResponse::error(None, "missing or empty 'type' field"));
    }

    if session_id.is_empty() {
        tracing::info!(type = type_str, "ram-a-kv event received");
    } else {
        tracing::info!(type = type_str, session_id = session_id, "ram-a-kv event received");
    }

    // list_events is a meta-event: bypass handlers and return every registered spec.
    if type_str == "list_events" {
        let events = state.registry.list_events();
        let events_map: serde_json::Map<String, Value> = events
            .iter()
            .map(|(name, spec)| {
                (
                    name.clone(),
                    serde_json::json!({
                        "description": spec.description,
                        "required": spec.required,
                        "optional": spec.optional,
                    }),
                )
            })
            .collect();
        return Json(EventResponse::ok(
            "list_events",
            serde_json::json!({ "events": events_map }),
        ));
    }

    // Resolve the handler once and dispatch directly. The previous code called
    // get_handler().is_none() and then get_handler().unwrap(), which always
    // re-locked the registry map.
    let Some(handler) = state.registry.get_handler(type_str) else {
        tracing::warn!(type = type_str, "event rejected: unknown event type");
        return Json(EventResponse::error(Some(type_str), "unknown event type"));
    };

    // Reject early if any field declared required by the spec is missing.
    if let Some(spec) = state.registry.get_spec(type_str) {
        for field in &spec.required {
            if payload.get(field).is_none() {
                tracing::warn!(type = type_str, field = field, "event rejected: missing required field");
                return Json(EventResponse::error(
                    Some(type_str),
                    &format!("missing required field '{}'", field),
                ));
            }
        }
    }

    let result = handler.handle(&payload, &state.context).await;

    match result {
        EventResult::Ok(data) => {
            if session_id.is_empty() {
                tracing::info!(type = type_str, status = 200, "ram-a-kv event completed");
            } else {
                tracing::info!(type = type_str, status = 200, session_id = session_id, "ram-a-kv event completed");
            }
            // Persist session state on turn_end so it survives daemon restarts.
            // Use the actual turn_count from the manager so the counter is not
            // reset to 0 on every save (the previous code hard-coded 0).
            if type_str == "turn_end" && !session_id.is_empty() {
                if let Some(map) = state.manager.session_map(session_id).await {
                    let turn_count = state
                        .manager
                        .session_turn_count(session_id)
                        .await
                        .unwrap_or(0);
                    if let Err(e) = state.session_store.save(session_id, &map, turn_count) {
                        tracing::warn!(error = %e, "session persistence failed");
                        // Surface the persistence failure so callers know state is
                        // not durably saved; the in-memory update still succeeded.
                        return Json(EventResponse::error(
                            Some(type_str),
                            &format!("event completed but persistence failed: {e}"),
                        ));
                    }
                }
            }
            // Drop persisted state on session_close; pinned sessions keep their
            // row for snapshot restore.
            if type_str == "session_close" && !session_id.is_empty() {
                if state.session_store.is_pinned(session_id) {
                    tracing::info!(
                        session_id = session_id,
                        "session_close: pinned, keeping SQLite row"
                    );
                } else {
                    state.session_store.remove(session_id);
                }
            }
            state.manager.log_snapshot().await;
            Json(EventResponse::ok(type_str, data))
        }
        EventResult::Err(e) => {
            tracing::warn!(type = type_str, error = %e, "ram-a-kv event failed");
            state.manager.log_snapshot().await;
            Json(EventResponse::error(Some(type_str), &e))
        }
    }
}

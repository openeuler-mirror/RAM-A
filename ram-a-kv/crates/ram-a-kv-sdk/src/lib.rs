mod config;

use serde_json::Value;
use std::sync::OnceLock;

pub struct RamAKvClient;

struct Inner {
    daemon_url: String,
    auth_token: String,
    http: reqwest::Client,
}

static GLOBAL: OnceLock<Option<Inner>> = OnceLock::new();

impl RamAKvClient {
    pub fn init_from_config() {
        Self::init_from_path(&config::default_config_path());
    }

    pub fn init_from_path(path: &std::path::Path) {
        GLOBAL.get_or_init(|| {
            config::load_config(path).map(|cfg| {
                let http = reqwest::Client::new();
                Inner {
                    daemon_url: cfg.daemon_url,
                    auth_token: cfg.auth_token,
                    http,
                }
            })
        });
    }

    pub fn is_enabled() -> bool {
        GLOBAL.get().map(|opt| opt.is_some()).unwrap_or(false)
    }

    fn emit(event_type: &str, payload: Value) {
        if let Some(Some(inner)) = GLOBAL.get() {
            let mut body = payload;
            if let Some(map) = body.as_object_mut() {
                map.insert("type".to_string(), Value::String(event_type.to_string()));
            }
            let url = format!("{}/event", inner.daemon_url);
            let http = inner.http.clone();
            let et = event_type.to_string();
            let auth_token = inner.auth_token.clone();
            tokio::spawn(async move {
                let mut req = http.post(&url).json(&body);
                if !auth_token.is_empty() {
                    req = req.bearer_auth(&auth_token);
                }
                match req.send().await {
                    Ok(resp) => {
                        tracing::info!(type = %et, status = %resp.status(), "ram-a-kv event ok");
                    }
                    Err(e) => {
                        tracing::warn!(type = %et, error = %e, "ram-a-kv event failed");
                    }
                }
            });
        }
    }

    pub fn turn_start(session_id: &str) {
        Self::emit("turn_start", serde_json::json!({"session_id": session_id}));
    }

    pub fn turn_end(
        session_id: &str,
        kv_transfer_params: Option<&Value>,
        debug_context: Option<&Value>,
    ) {
        let mut p = serde_json::json!({"session_id": session_id});
        if let Some(v) = kv_transfer_params {
            p["kv_transfer_params"] = v.clone();
        }
        if let Some(v) = debug_context {
            p["debug_context"] = v.clone();
        }
        Self::emit("turn_end", p);
    }

    pub fn snapshot_restore(session_id: &str) {
        Self::emit(
            "snapshot_restore",
            serde_json::json!({"session_id": session_id}),
        );
    }

    pub fn session_close(session_id: &str) {
        Self::emit(
            "session_close",
            serde_json::json!({"session_id": session_id}),
        );
    }

    pub fn session_suspend(session_id: &str) {
        Self::emit(
            "session_suspend",
            serde_json::json!({"session_id": session_id}),
        );
    }

    pub fn session_fork(session_id: &str) {
        Self::emit(
            "session_fork",
            serde_json::json!({"session_id": session_id}),
        );
    }

    pub fn session_fork_end(session_id: &str) {
        Self::emit(
            "session_fork_end",
            serde_json::json!({"session_id": session_id}),
        );
    }
}

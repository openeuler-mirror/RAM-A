// ram-a-kv daemon entry point: load config, build backend, restore sessions,
// register event handlers, and serve HTTP.

pub mod api;
mod backend;
mod daemon_config;
mod event_registry;
mod handlers;
mod session_store;

use std::sync::Arc;

use manager_core::backend::noop::NoopBackend;
use manager_core::backend::KvCacheBackend;
use manager_core::config::KvCacheConfig;
use manager_core::KvCacheManager;

use crate::backend::lmcache_ascend::LMCacheAscendBackend;
use crate::daemon_config::DaemonConfig;
use crate::event_registry::AppContext;
use crate::event_registry::EventRegistry;
use crate::handlers::health::HealthHandler;
use crate::handlers::session_close::SessionCloseHandler;
use crate::handlers::session_fork::SessionForkHandler;
use crate::handlers::session_fork_end::SessionForkEndHandler;
use crate::handlers::session_map::SessionMapHandler;
use crate::handlers::session_suspend::SessionSuspendHandler;
use crate::handlers::snapshot_restore::SnapshotRestoreHandler;
use crate::handlers::turn_end::TurnEndHandler;
use crate::handlers::turn_start::TurnStartHandler;

use api::AppState;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use tokio::net::TcpListener;

// Load config from $RAM_A_KV_CONFIG (default /etc/ram-a-kv/config.toml); fall back to defaults on any error.
fn load_config() -> DaemonConfig {
    let config_path = std::env::var("RAM_A_KV_CONFIG")
        .unwrap_or_else(|_| "/etc/ram-a-kv/config.toml".to_string());
    if std::path::Path::new(&config_path).exists() {
        let content = std::fs::read_to_string(&config_path)
            .unwrap_or_else(|e| {
                tracing::warn!(path = %config_path, error = %e, "config file read failed, using defaults");
                String::new()
            });
        DaemonConfig::from_toml_str(&content).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "config parse failed, using defaults");
            DaemonConfig::default()
        })
    } else {
        tracing::info!("no config file found, using defaults");
        DaemonConfig::default()
    }
}

// Returns true when `listen_addr` binds to a non-loopback interface, i.e. the
// HTTP endpoint is reachable from other hosts. Used to enforce that an
// `auth_token` is configured before such an exposure is allowed.
fn is_loopback_listen_addr(listen_addr: &str) -> bool {
    // Strip port: accept "127.0.0.1:port", "localhost:port", "[::1]:port", "::1:port".
    let host = listen_addr
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(listen_addr);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "")
}

// Bearer token authentication middleware. When `auth_token` is non-empty, all
// requests must carry `Authorization: Bearer <token>`. Loopback-only daemons
// with no token configured still pass through.
async fn require_auth(
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let expected = &app_state.auth_token;
    if !expected.is_empty() {
        let provided = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        match provided {
            Some(token) if token == expected => {}
            _ => {
                return (StatusCode::UNAUTHORIZED, "invalid or missing bearer token")
                    .into_response();
            }
        }
    }
    next.run(request).await
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let config = load_config();

    // Refuse to start when the daemon would expose an unauthenticated /event
    // endpoint to non-loopback callers.
    if !is_loopback_listen_addr(&config.listen_addr) && config.auth_token.is_empty() {
        tracing::error!(
            addr = %config.listen_addr,
            "refusing to start: non-loopback listen_addr requires auth_token to be set"
        );
        std::process::exit(1);
    }

    // Pick backend: NoopBackend for tests (no LMCache traffic) vs. LMCacheAscendBackend for production.
    let backend: Arc<dyn KvCacheBackend> = if config.noop_backend {
        tracing::info!("using noop backend (no LMCache calls)");
        Arc::new(NoopBackend)
    } else {
        tracing::info!(url = %config.lmcache_url, "using LMCache-Ascend backend");
        Arc::new(LMCacheAscendBackend::new(config.lmcache_url.clone()))
    };

    let core_config = KvCacheConfig {
        enabled: true,
        debug_enabled: config.debug_enabled,
        trace_events: config.trace_events,
        debug_dir: config.debug_dir.clone(),
        backend_url: config.lmcache_url.clone(),
        turn_start_prefetch: config.turn_start_prefetch,
    };
    let manager = Arc::new(KvCacheManager::new(core_config, backend));

    let session_store = Arc::new(
        crate::session_store::SqliteSessionStore::new(&config.session_store_path).unwrap_or_else(
            |e| {
                tracing::error!(error = %e, "session store init failed");
                panic!("cannot initialize session store")
            },
        ),
    );

    // Rehydrate every persisted session so state survives daemon restarts.
    for session_id in session_store.list_all() {
        if let Some((map, turn_count)) = session_store.load(&session_id) {
            manager
                .restore_session(
                    &session_id,
                    manager_core::manager::SessionKvState { map, turn_count },
                )
                .await;
        }
    }
    // Rebuild global refcounts from the restored maps so shared chunks keep the
    // correct reference count after a daemon restart.
    manager.rebuild_refcounts().await;

    let mut registry = EventRegistry::new();
    registry.register("turn_start", Arc::new(TurnStartHandler));
    registry.register("turn_end", Arc::new(TurnEndHandler));
    registry.register("snapshot_restore", Arc::new(SnapshotRestoreHandler));
    registry.register("session_map", Arc::new(SessionMapHandler));
    registry.register("session_close", Arc::new(SessionCloseHandler));
    registry.register("session_suspend", Arc::new(SessionSuspendHandler));
    registry.register("session_fork", Arc::new(SessionForkHandler));
    registry.register("session_fork_end", Arc::new(SessionForkEndHandler));
    registry.register("health", Arc::new(HealthHandler));

    let context = Arc::new(AppContext {
        manager: manager.clone(),
        session_store: session_store.clone(),
    });

    let app_state = Arc::new(AppState {
        manager,
        registry: Arc::new(registry),
        session_store,
        context,
        auth_token: config.auth_token.clone(),
    });

    // Single route: everything flows through POST /event and the registry dispatches by "type".
    let app = Router::new()
        .route("/event", post(api::handle_event))
        .with_state(app_state.clone())
        .layer(middleware::from_fn_with_state(app_state, require_auth));

    tracing::info!(addr = %config.listen_addr, noop = config.noop_backend, "ram-a-kv daemon starting");
    let listener = TcpListener::bind(&config.listen_addr)
        .await
        .unwrap_or_else(|e| {
            tracing::error!(addr = %config.listen_addr, error = %e, "failed to bind listen address");
            panic!("cannot bind listen address {}", config.listen_addr)
        });
    axum::serve(listener, app).await.unwrap_or_else(|e| {
        tracing::error!(error = %e, "server terminated with error");
        std::process::exit(1);
    });
}

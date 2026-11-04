// Copyright (c) 2026 Huawei Technologies Co.,Ltd. All rights reserved.
//
// RAM-A is licensed under Mulan PSL v2.
// You can use this software according to the terms and conditions of the Mulan
// PSL v2.
// You may obtain a copy of Mulan PSL v2 at:
//         http://license.coscl.org.cn/MulanPSL2
// THIS SOFTWARE IS PROVIDED ON AN "AS IS" BASIS, WITHOUT WARRANTIES OF ANY
// KIND, EITHER EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO
// NON-INFRINGEMENT, MERCHANTABILITY OR FIT FOR A PARTICULAR PURPOSE.
// See the Mulan PSL v2 for more details.

// Session persistence: stores each session's KvCacheMap and turn_count.
//
// Two implementations behind one `SessionStore` trait:
// - `MemorySessionStore` (default, always available): process-local state, no
//   external dependency and no files. Used unless the `sqlite` feature is on.
// - `SqliteSessionStore` (opt-in feature `sqlite`): durable, survives daemon
//   restarts. Only compiled when the feature is enabled via `--features
//   sqlite`, so the default build has no rusqlite/libsqlite3-sys dependency.

use manager_core::KvCacheMap;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Persistence backend used by the daemon for session state.
///
/// Synchronous by design: SQLite is blocking, and callers already run these
/// operations on the request path or the startup recovery path.
pub trait SessionStore: Send + Sync {
    fn save(
        &self,
        session_id: &str,
        map: &KvCacheMap,
        turn_count: u32,
    ) -> Result<(), SessionStoreError>;
    fn load(&self, session_id: &str) -> Option<(KvCacheMap, u32)>;
    fn remove(&self, session_id: &str);
    fn list_all(&self) -> Vec<String>;
    fn pin_session(&self, session_id: &str);
    fn is_pinned(&self, session_id: &str) -> bool;
}

// Persistence error: returned by `save` so callers can decide whether to
// surface the failure to the API response (e.g., turn_end should report
// degraded state when persistence fails).
#[derive(Debug, thiserror::Error)]
pub enum SessionStoreError {
    #[cfg(feature = "sqlite")]
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// In-memory session store: no SQLite, no files. State is lost when the
/// process exits, so restart recovery is a no-op.
///
/// Only constructed by non-sqlite builds and by tests; a `--features sqlite`
/// daemon always uses `SqliteSessionStore`.
#[cfg_attr(feature = "sqlite", allow(dead_code))]
#[derive(Default)]
pub struct MemorySessionStore {
    sessions: Mutex<HashMap<String, (KvCacheMap, u32)>>,
    pins: Mutex<HashSet<String>>,
}

#[cfg_attr(feature = "sqlite", allow(dead_code))]
impl MemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SessionStore for MemorySessionStore {
    fn save(
        &self,
        session_id: &str,
        map: &KvCacheMap,
        turn_count: u32,
    ) -> Result<(), SessionStoreError> {
        self.sessions
            .lock()
            .unwrap()
            .insert(session_id.to_string(), (map.clone(), turn_count));
        Ok(())
    }

    fn load(&self, session_id: &str) -> Option<(KvCacheMap, u32)> {
        self.sessions.lock().unwrap().get(session_id).cloned()
    }

    fn remove(&self, session_id: &str) {
        self.sessions.lock().unwrap().remove(session_id);
        self.pins.lock().unwrap().remove(session_id);
    }

    fn list_all(&self) -> Vec<String> {
        self.sessions.lock().unwrap().keys().cloned().collect()
    }

    fn pin_session(&self, session_id: &str) {
        self.pins.lock().unwrap().insert(session_id.to_string());
    }

    fn is_pinned(&self, session_id: &str) -> bool {
        self.pins.lock().unwrap().contains(session_id)
    }
}

/// Create the session store selected at build time.
///
/// With the `sqlite` feature the returned store persists to `path`; without it
/// the path is ignored and an in-memory store is returned.
pub fn new_session_store(path: &str) -> Result<Arc<dyn SessionStore>, SessionStoreError> {
    #[cfg(feature = "sqlite")]
    {
        Ok(Arc::new(SqliteSessionStore::new(path)?))
    }
    #[cfg(not(feature = "sqlite"))]
    {
        let _ = path;
        tracing::warn!(
            "built without the 'sqlite' feature; using in-memory session store \
             (state will not survive a daemon restart)"
        );
        Ok(Arc::new(MemorySessionStore::new()))
    }
}

#[cfg(feature = "sqlite")]
mod sqlite_store {
    use super::{KvCacheMap, SessionStore, SessionStoreError};
    use rusqlite::Connection;
    use std::sync::Mutex;

    pub struct SqliteSessionStore {
        // SQLite is synchronous, so guard the connection with std::sync::Mutex.
        db: Mutex<Connection>,
    }

    impl SqliteSessionStore {
        // Mutex policy: `load`/`list_all` are on the daemon restart recovery path,
        // so they tolerate a poisoned lock (log + return empty) rather than abort
        // the recovery. `save`/`remove`/`pin_session`/`is_pinned` are on the request
        // path and keep panic-on-poison (Rust idiom): a poisoned lock means another
        // thread already panicked and daemon state is suspect.
        // Open (or create) the DB file and ensure the sessions table exists.
        // Schema: sessions(session_id TEXT PK, map_json TEXT NOT NULL, turn_count INTEGER NOT NULL DEFAULT 0)
        pub fn new(path: &str) -> Result<Self, rusqlite::Error> {
            if let Some(parent) = std::path::Path::new(path).parent() {
                if !parent.as_os_str().is_empty() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        tracing::warn!(path = %parent.display(), error = %e, "failed to create session store directory");
                    }
                }
            }
            let db = Connection::open(path)?;
            db.execute_batch(
                "CREATE TABLE IF NOT EXISTS sessions (
                    session_id TEXT PRIMARY KEY,
                    map_json TEXT NOT NULL,
                    turn_count INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE IF NOT EXISTS pins (
                    session_id TEXT PRIMARY KEY,
                    pinned_at_ms INTEGER NOT NULL
                );",
            )?;
            Ok(Self { db: Mutex::new(db) })
        }
    }

    impl SessionStore for SqliteSessionStore {
        // Persist the session map and turn_count. Returns an error so the caller can
        // surface persistence failures (previously the error was only logged and the
        // API still reported success).
        fn save(
            &self,
            session_id: &str,
            map: &KvCacheMap,
            turn_count: u32,
        ) -> Result<(), SessionStoreError> {
            let map_json = serde_json::to_string(map)?;
            let db = self.db.lock().unwrap();
            db.execute(
                "INSERT OR REPLACE INTO sessions (session_id, map_json, turn_count) VALUES (?1, ?2, ?3)",
                rusqlite::params![session_id, map_json, turn_count],
            )?;
            Ok(())
        }

        // Returns (KvCacheMap, turn_count), or None if the row is missing.
        fn load(&self, session_id: &str) -> Option<(KvCacheMap, u32)> {
            let db = match self.db.lock() {
                Ok(g) => g,
                Err(e) => {
                    tracing::error!(session_id = %session_id, error = %e, "load: store mutex poisoned");
                    return None;
                }
            };
            let mut stmt = match db
                .prepare("SELECT map_json, turn_count FROM sessions WHERE session_id = ?1")
            {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(session_id = %session_id, error = %e, "load: prepare failed");
                    return None;
                }
            };
            let row = match stmt.query_row(rusqlite::params![session_id], |row| {
                let map_json: String = row.get(0)?;
                let turn_count: u32 = row.get(1)?;
                Ok((map_json, turn_count))
            }) {
                Ok(r) => r,
                Err(rusqlite::Error::QueryReturnedNoRows) => return None,
                Err(e) => {
                    tracing::error!(session_id = %session_id, error = %e, "load: query failed");
                    return None;
                }
            };
            let (map_json, turn_count) = row;
            match serde_json::from_str::<KvCacheMap>(&map_json) {
                Ok(map) => Some((map, turn_count)),
                Err(e) => {
                    tracing::error!(
                        session_id = %session_id,
                        error = %e,
                        "load: map_json deserialization failed, row is corrupt"
                    );
                    None
                }
            }
        }

        fn remove(&self, session_id: &str) {
            let db = self.db.lock().unwrap();
            if let Err(e) = db.execute(
                "DELETE FROM sessions WHERE session_id = ?1",
                rusqlite::params![session_id],
            ) {
                tracing::warn!(error = %e, "session remove failed");
            }
        }

        // List all persisted session IDs. Returns an empty vec on failure (the
        // previous implementation panicked on a DB error, taking the whole daemon down).
        fn list_all(&self) -> Vec<String> {
            let db = match self.db.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    tracing::error!(error = %e, "session store mutex poisoned");
                    return Vec::new();
                }
            };
            let mut stmt = match db.prepare("SELECT session_id FROM sessions") {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "list_all prepare failed");
                    return Vec::new();
                }
            };
            stmt.query_map([], |row| row.get::<_, String>(0))
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
                .unwrap_or_default()
        }

        // Pin a session so its SQLite row survives session_close (snapshot restore
        // still needs the chunk list).
        fn pin_session(&self, session_id: &str) {
            let pinned_at_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let db = self.db.lock().unwrap();
            match db.execute(
                "INSERT OR REPLACE INTO pins (session_id, pinned_at_ms) VALUES (?1, ?2)",
                rusqlite::params![session_id, pinned_at_ms],
            ) {
                Ok(_) => tracing::info!(session_id = %session_id, "session pinned"),
                Err(e) => tracing::warn!(session_id = %session_id, error = %e, "session pin failed"),
            }
        }

        // Whether the session is pinned (has a snapshot).
        fn is_pinned(&self, session_id: &str) -> bool {
            let db = self.db.lock().unwrap();
            db.query_row(
                "SELECT 1 FROM pins WHERE session_id = ?1",
                rusqlite::params![session_id],
                |_| Ok(()),
            )
            .is_ok()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn tmp_store(name: &str) -> SqliteSessionStore {
            // Each test gets a unique path so parallel `cargo test` runs don't
            // trip over each other removing/opening the same db file.
            let path = format!("/tmp/ram-a-kv-test-{name}-{}.db", std::process::id());
            let _ = std::fs::remove_file(&path);
            SqliteSessionStore::new(&path).expect("open test store")
        }

        #[test]
        fn load_returns_none_for_missing_session() {
            let store = tmp_store("missing");
            assert!(store.load("nope").is_none());
        }

        #[test]
        fn load_returns_none_for_corrupt_json_row() {
            // Restart recovery must not panic when a row's map_json is not valid
            // JSON (e.g., file corruption). The row is left in place; load just
            // returns None and emits an error log.
            let store = tmp_store("corrupt");
            // Insert a well-formed row first so the table is set up.
            store
                .save("s1", &KvCacheMap(vec!["A".into()]), 1)
                .expect("save");
            // Corrupt the row directly via the connection.
            {
                let db = store.db.lock().unwrap();
                db.execute(
                    "UPDATE sessions SET map_json = ?1 WHERE session_id = ?2",
                    rusqlite::params!["not json", "s1"],
                )
                .expect("corrupt row");
            }
            assert!(
                store.load("s1").is_none(),
                "corrupt row must yield None, not panic"
            );
        }

        #[test]
        fn save_and_load_round_trip() {
            let store = tmp_store("roundtrip");
            store
                .save("s1", &KvCacheMap(vec!["A".into(), "B".into()]), 7)
                .expect("save");
            let (map, turn_count) = store.load("s1").expect("load");
            assert_eq!(map.chunk_hashes(), vec!["A".to_string(), "B".to_string()]);
            assert_eq!(turn_count, 7);
        }
    }
}

#[cfg(feature = "sqlite")]
pub use sqlite_store::SqliteSessionStore;

#[cfg(test)]
mod memory_tests {
    use super::*;

    #[test]
    fn save_load_remove_round_trip() {
        let store = MemorySessionStore::new();
        assert!(store.load("s1").is_none());
        store
            .save("s1", &KvCacheMap(vec!["A".into(), "B".into()]), 3)
            .expect("save");
        let (map, turn_count) = store.load("s1").expect("load");
        assert_eq!(map.chunk_hashes(), vec!["A".to_string(), "B".to_string()]);
        assert_eq!(turn_count, 3);
        assert_eq!(store.list_all(), vec!["s1".to_string()]);

        store.remove("s1");
        assert!(store.load("s1").is_none());
        assert!(store.list_all().is_empty());
    }

    #[test]
    fn pin_state_is_tracked() {
        let store = MemorySessionStore::new();
        assert!(!store.is_pinned("s1"));
        store.pin_session("s1");
        assert!(store.is_pinned("s1"));
        store.remove("s1");
        assert!(!store.is_pinned("s1"));
    }
}

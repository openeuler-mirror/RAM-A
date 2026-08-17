// SQLite-backed session persistence: stores each session's KvCacheMap and turn_count
// so state survives daemon restarts. Saved on turn_end, removed on session_close/delete.

use manager_core::KvCacheMap;
use rusqlite::Connection;
use std::sync::Mutex;

pub struct SqliteSessionStore {
    // SQLite is synchronous, so guard the connection with std::sync::Mutex.
    db: Mutex<Connection>,
}

// Persistence error: returned by `save`/`list_all` so callers can decide whether
// to surface the failure to the API response (e.g., turn_end should report
// degraded state when persistence fails).
#[derive(Debug, thiserror::Error)]
pub enum SessionStoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl SqliteSessionStore {
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

    // Persist the session map and turn_count. Returns an error so the caller can
    // surface persistence failures (previously the error was only logged and the
    // API still reported success).
    pub fn save(
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

    // Returns (KvCacheMap, turn_count), or None if the row is missing or JSON parsing fails.
    pub fn load(&self, session_id: &str) -> Option<(KvCacheMap, u32)> {
        let db = self.db.lock().unwrap();
        let mut stmt = db
            .prepare("SELECT map_json, turn_count FROM sessions WHERE session_id = ?1")
            .ok()?;
        stmt.query_row(rusqlite::params![session_id], |row| {
            let map_json: String = row.get(0)?;
            let turn_count: u32 = row.get(1)?;
            Ok((map_json, turn_count))
        })
        .ok()
        .and_then(|(map_json, turn_count)| {
            serde_json::from_str(&map_json)
                .ok()
                .map(|map| (map, turn_count))
        })
    }

    pub fn remove(&self, session_id: &str) {
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
    pub fn list_all(&self) -> Vec<String> {
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
    pub fn pin_session(&self, session_id: &str) {
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
    pub fn is_pinned(&self, session_id: &str) -> bool {
        let db = self.db.lock().unwrap();
        db.query_row(
            "SELECT 1 FROM pins WHERE session_id = ?1",
            rusqlite::params![session_id],
            |_| Ok(()),
        )
        .is_ok()
    }
}

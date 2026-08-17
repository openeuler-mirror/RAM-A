// KvCacheManager: core KV cache coordinator.
// Handles session management, cross-session refcounting, and backend interaction.
// SAFETY: a chunk is evicted only when no session references it (global refcount == 0).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::backend::noop::NoopBackend;
use crate::backend::{BackendResult, KvCacheBackend};
use crate::config::KvCacheConfig;
use crate::debug::{write_debug_file, DebugContext};
use crate::error::{KvCacheError, ManagerResult};
use crate::map::KvCacheMap;

pub struct KvCacheManager {
    backend: Arc<dyn KvCacheBackend>,
    config: KvCacheConfig,
    sessions: Mutex<HashMap<String, SessionKvState>>,
    // Global refcount: chunk_hash -> number of sessions referencing it.
    // SAFETY: only refcount==0 chunks are evicted, so shared chunks are never dropped early.
    refcounts: Mutex<HashMap<String, u64>>,
    // Active fork snapshots keyed by fork_id. Each snapshot records the parent's
    // hash set at fork time so `fork_end` releases exactly those references even
    // if the parent's map is replaced in the meantime.
    fork_snapshots: Mutex<HashMap<u64, ForkSnapshot>>,
    // Monotonic fork-id generator.
    next_fork_id: AtomicU64,
}

// Per-session KV cache state.
pub struct SessionKvState {
    // Chunk hashes currently referenced by this session.
    pub map: KvCacheMap,
    // Number of completed inference turns.
    pub turn_count: u32,
}

// Captured snapshot of a parent session's chunk hashes at fork time.
// Used so `fork_end` decrements exactly the hashes the fork incremented.
struct ForkSnapshot {
    session_id: String,
    hashes: Vec<String>,
}

// Structured outcome of a mutating operation. Handlers use these fields to
// build honest responses instead of returning hard-coded values.
#[derive(Debug, Default, Clone)]
pub struct OperationOutcome {
    // Number of chunk hashes evicted from the backend during this operation.
    pub evicted_count: usize,
    // True when a debug file was actually written.
    pub debug_written: bool,
    // True when a prefetch request was sent to the backend.
    pub prefetch_sent: bool,
    // Number of chunk hashes included in the prefetch request.
    pub prefetch_count: usize,
    // True when the backend reported a non-success status or did not respond.
    // State has still been updated; callers may use this to mark degraded responses.
    pub backend_degraded: bool,
}

// Outcome of `on_session_fork`. Includes the fork_id callers must pass back to
// `on_session_fork_end` so the correct snapshot is consumed.
#[derive(Debug, Clone)]
pub struct ForkOutcome {
    pub fork_id: u64,
}

// Returns the unique hashes from the input, preserving first-seen order.
fn unique_hashes(hashes: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for h in hashes {
        if seen.insert(h.clone()) {
            out.push(h.clone());
        }
    }
    out
}

// True when the backend result indicates a successful 2xx response.
fn backend_ok(result: &BackendResult) -> bool {
    result.responded && result.status >= 200 && result.status < 300
}

impl KvCacheManager {
    // Create a coordinator with the given config and backend.
    pub fn new(config: KvCacheConfig, backend: Arc<dyn KvCacheBackend>) -> Self {
        Self {
            backend,
            config,
            sessions: Mutex::new(HashMap::new()),
            refcounts: Mutex::new(HashMap::new()),
            fork_snapshots: Mutex::new(HashMap::new()),
            next_fork_id: AtomicU64::new(1),
        }
    }

    // Create a coordinator backed by NoopBackend (for tests/dev).
    pub fn new_noop(config: KvCacheConfig) -> Self {
        Self::new(config, Arc::new(NoopBackend))
    }

    // Pre-inference prefetch: send the session's current chunk hashes to the backend.
    // Skips prefetch when the session is unknown (new session has no cache yet) or
    // when `config.turn_start_prefetch` is false.
    //
    // The session lock is released before the backend call so a slow backend cannot
    // block other sessions.
    pub async fn on_turn_start(&self, session_id: &str) -> ManagerResult<OperationOutcome> {
        let mut outcome = OperationOutcome::default();
        if !self.config.turn_start_prefetch {
            return Ok(outcome);
        }
        let hashes = {
            let sessions = self.sessions.lock().await;
            sessions
                .get(session_id)
                .map(|s| unique_hashes(&s.map.chunk_hashes()))
                .unwrap_or_default()
        };
        if hashes.is_empty() {
            return Ok(outcome);
        }
        outcome.prefetch_count = hashes.len();
        let result = self
            .backend
            .prefetch(hashes, format!("turn_prefetch_{}", session_id))
            .await;
        if !backend_ok(&result) {
            tracing::warn!(
                status = result.status,
                responded = result.responded,
                "LMCache turn_start prefetch failed"
            );
            outcome.backend_degraded = true;
        } else {
            outcome.prefetch_sent = true;
        }
        Ok(outcome)
    }

    // Post-inference update: apply the new chunk-hash set, adjust refcounts, and evict
    // chunks that are no longer referenced by any session.
    //
    // Algorithm:
    // 1. Auto-create an empty session if it does not exist.
    // 2. Compute unique old/new hash sets. Duplicate hashes inside one session do
    //    NOT inflate refcount ("each session, each unique hash once").
    // 3. Decrement refcount for hashes no longer referenced, increment for new ones.
    // 4. Hashes whose refcount drops to 0 are evicted (outside the locks).
    // 5. Replace the session's map with the new hash set (input order preserved).
    // 6. Optionally write a debug file (outside the locks).
    pub async fn on_turn_end(
        &self,
        session_id: &str,
        new_hashes: Vec<String>,
        debug_context: Option<DebugContext>,
    ) -> ManagerResult<OperationOutcome> {
        let mut outcome = OperationOutcome::default();

        let (to_evict, turn_count) = {
            let mut sessions = self.sessions.lock().await;
            if !sessions.contains_key(session_id) {
                sessions.insert(
                    session_id.to_string(),
                    SessionKvState {
                        map: KvCacheMap::default(),
                        turn_count: 0,
                    },
                );
            }
            let state = sessions
                .get_mut(session_id)
                .ok_or(KvCacheError::SessionNotFound {
                    session_id: session_id.to_string(),
                })?;

            let old_hashes = state.map.chunk_hashes();
            let old_unique = unique_hashes(&old_hashes);
            let new_unique = unique_hashes(&new_hashes);
            let old_set: HashSet<&String> = old_unique.iter().collect();
            let new_set: HashSet<&String> = new_unique.iter().collect();

            let to_decrement: Vec<String> = old_unique
                .iter()
                .filter(|h| !new_set.contains(h))
                .cloned()
                .collect();
            let to_increment: Vec<String> = new_unique
                .iter()
                .filter(|h| !old_set.contains(h))
                .cloned()
                .collect();

            let mut refcounts = self.refcounts.lock().await;
            let mut to_evict = Vec::new();
            for hash in &to_decrement {
                if let Some(count) = refcounts.get_mut(hash) {
                    *count -= 1;
                    if *count == 0 {
                        to_evict.push(hash.clone());
                        refcounts.remove(hash);
                    }
                }
            }
            for hash in &to_increment {
                *refcounts.entry(hash.clone()).or_insert(0) += 1;
            }
            drop(refcounts);

            state.map.replace(&new_hashes);
            state.turn_count += 1;
            (to_evict, state.turn_count)
        };

        if !to_evict.is_empty() {
            let result = self.backend.evict(to_evict.clone()).await;
            if !backend_ok(&result) {
                tracing::warn!(
                    status = result.status,
                    responded = result.responded,
                    count = to_evict.len(),
                    "LMCache evict failed during turn_end"
                );
                outcome.backend_degraded = true;
            } else {
                outcome.evicted_count = to_evict.len();
            }
        }

        if self.config.debug_enabled {
            // Always write a debug file when debug_enabled, even if the request
            // carried no chunk_hashes or no debug_context. Missing fields are
            // filled with empty defaults so the file shape stays consistent.
            let ctx = debug_context.unwrap_or_default();
            match write_debug_file(&self.config, session_id, turn_count, &new_hashes, &ctx) {
                Ok(()) => outcome.debug_written = true,
                Err(e) => {
                    tracing::warn!(error = %e, "debug file write failed");
                    return Err(e);
                }
            }
        }

        Ok(outcome)
    }

    // Snapshot restore: accept an ordered chunk-hash vector and atomically replace the
    // target session's map.
    //
    // Idempotent: when the target session already exists, the diff between the old and
    // new hash sets is applied so calling restore twice does not double-count references.
    // `turn_count` is preserved across restore.
    //
    // Prefetch and session storage both preserve the client-supplied order (no HashMap
    // conversion). Refcounts are incremented per unique hash (local dedup).
    pub async fn on_snapshot_restore(
        &self,
        session_id: &str,
        hashes: Vec<String>,
    ) -> ManagerResult<OperationOutcome> {
        self.on_snapshot_restore_from(session_id, session_id, hashes)
            .await
    }

    // Restore `target_session_id` from a snapshot read out of `source_session_id`.
    // Used by session resume flows where the new session must inherit references from
    // the previous one.
    pub async fn on_snapshot_restore_from(
        &self,
        source_session_id: &str,
        target_session_id: &str,
        hashes: Vec<String>,
    ) -> ManagerResult<OperationOutcome> {
        let mut outcome = OperationOutcome::default();
        let new_unique = unique_hashes(&hashes);
        let mut new_map = KvCacheMap::default();
        new_map.replace(&hashes);

        let to_evict = {
            let mut sessions = self.sessions.lock().await;
            let old_hashes = sessions
                .get(target_session_id)
                .map(|s| unique_hashes(&s.map.chunk_hashes()))
                .unwrap_or_default();
            let old_set: HashSet<&String> = old_hashes.iter().collect();
            let new_set: HashSet<&String> = new_unique.iter().collect();

            let to_decrement: Vec<String> = old_unique_iter(&old_hashes, &new_set);
            let to_increment: Vec<String> = new_unique
                .iter()
                .filter(|h| !old_set.contains(h))
                .cloned()
                .collect();

            let mut refcounts = self.refcounts.lock().await;
            let mut to_evict = Vec::new();
            for hash in &to_decrement {
                if let Some(count) = refcounts.get_mut(hash) {
                    *count -= 1;
                    if *count == 0 {
                        to_evict.push(hash.clone());
                        refcounts.remove(hash);
                    }
                }
            }
            for hash in &to_increment {
                *refcounts.entry(hash.clone()).or_insert(0) += 1;
            }
            drop(refcounts);

            // Preserve turn_count so restore is not a reset.
            let turn_count = sessions
                .get(target_session_id)
                .map(|s| s.turn_count)
                .unwrap_or(0);
            sessions.insert(
                target_session_id.to_string(),
                SessionKvState {
                    map: new_map,
                    turn_count,
                },
            );
            to_evict
        };

        if !to_evict.is_empty() {
            let result = self.backend.evict(to_evict.clone()).await;
            if !backend_ok(&result) {
                tracing::warn!(
                    status = result.status,
                    responded = result.responded,
                    count = to_evict.len(),
                    "LMCache evict failed during snapshot_restore"
                );
                outcome.backend_degraded = true;
            } else {
                outcome.evicted_count = to_evict.len();
            }
        }

        if !hashes.is_empty() {
            outcome.prefetch_count = hashes.len();
            let result = self
                .backend
                .prefetch(hashes, format!("snapshot_prefetch_{}", target_session_id))
                .await;
            if !backend_ok(&result) {
                tracing::warn!(
                    status = result.status,
                    responded = result.responded,
                    "LMCache snapshot_restore prefetch failed"
                );
                outcome.backend_degraded = true;
            } else {
                outcome.prefetch_sent = true;
            }
        }

        let _ = source_session_id; // source is read by the caller (handler), not here.
        Ok(outcome)
    }

    // Return a clone of the session's current cache map (does not mutate internal state).
    pub async fn session_map(&self, session_id: &str) -> Option<KvCacheMap> {
        self.sessions
            .lock()
            .await
            .get(session_id)
            .map(|s| s.map.clone())
    }

    // Return the session's turn_count (used for persistence so it does not reset on restart).
    pub async fn session_turn_count(&self, session_id: &str) -> Option<u32> {
        self.sessions
            .lock()
            .await
            .get(session_id)
            .map(|s| s.turn_count)
    }

    // Close a session: remove it, decrement refcounts for its unique hashes, evict any that hit 0.
    // Locks are released before the backend call.
    // Lenient on unknown sessions so the caller's SQLite cleanup can still proceed.
    pub async fn on_session_close(&self, session_id: &str) -> ManagerResult<OperationOutcome> {
        let mut outcome = OperationOutcome::default();

        let to_evict = {
            let mut sessions = self.sessions.lock().await;
            let Some(state) = sessions.remove(session_id) else {
                tracing::info!(session_id = %session_id, "session_close: not in memory, skip cleanup");
                return Ok(outcome);
            };
            let unique = unique_hashes(&state.map.chunk_hashes());

            let mut refcounts = self.refcounts.lock().await;
            let mut to_evict = Vec::new();
            for hash in &unique {
                if let Some(count) = refcounts.get_mut(hash) {
                    *count -= 1;
                    if *count == 0 {
                        to_evict.push(hash.clone());
                        refcounts.remove(hash);
                    }
                }
            }
            to_evict
        };

        if !to_evict.is_empty() {
            let result = self.backend.evict(to_evict.clone()).await;
            if !backend_ok(&result) {
                tracing::warn!(
                    status = result.status,
                    responded = result.responded,
                    count = to_evict.len(),
                    "LMCache evict failed during session_close"
                );
                outcome.backend_degraded = true;
            } else {
                outcome.evicted_count = to_evict.len();
            }
        }
        Ok(outcome)
    }

    // Fork a session: capture the parent's current hash set, generate a fork_id, and
    // increment refcount for every unique hash so the parent's later `turn_end` cannot
    // evict chunks still shared with the subagent.
    //
    // The returned fork_id must be passed to `on_session_fork_end` so the exact same
    // snapshot is released. This is a deliberate change from the previous behavior of
    // reading the parent's current map at fork_end, which could release the wrong set.
    pub async fn on_session_fork(&self, session_id: &str) -> ManagerResult<ForkOutcome> {
        let hashes = {
            let sessions = self.sessions.lock().await;
            let Some(state) = sessions.get(session_id) else {
                return Err(KvCacheError::SessionNotFound {
                    session_id: session_id.to_string(),
                });
            };
            unique_hashes(&state.map.chunk_hashes())
        };

        let fork_id = self.next_fork_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut snapshots = self.fork_snapshots.lock().await;
            snapshots.insert(
                fork_id,
                ForkSnapshot {
                    session_id: session_id.to_string(),
                    hashes: hashes.clone(),
                },
            );
        }
        {
            let mut refcounts = self.refcounts.lock().await;
            for hash in &hashes {
                *refcounts.entry(hash.clone()).or_insert(0) += 1;
            }
        }
        Ok(ForkOutcome { fork_id })
    }

    // End a fork: look up the saved snapshot (by fork_id when provided, otherwise the
    // latest snapshot for this session), decrement refcounts for its hashes, and evict
    // any that hit 0.
    //
    // Idempotent: returns Ok with no eviction when the snapshot is missing (e.g., a
    // duplicate fork_end) so callers cannot decrement the same reference twice.
    pub async fn on_session_fork_end(
        &self,
        session_id: &str,
        fork_id: Option<u64>,
    ) -> ManagerResult<OperationOutcome> {
        let mut outcome = OperationOutcome::default();

        let to_evict = {
            let mut snapshots = self.fork_snapshots.lock().await;
            let snapshot = if let Some(id) = fork_id {
                snapshots.remove(&id)
            } else {
                // No fork_id: pick the most recent fork for this session.
                // fork_id is monotonically increasing, so max == latest.
                snapshots
                    .iter()
                    .filter(|(_, snap)| snap.session_id == session_id)
                    .map(|(id, _)| *id)
                    .max()
                    .and_then(|id| snapshots.remove(&id))
            };

            let Some(snapshot) = snapshot else {
                return Ok(outcome);
            };

            let mut refcounts = self.refcounts.lock().await;
            let mut to_evict = Vec::new();
            for hash in &snapshot.hashes {
                if let Some(count) = refcounts.get_mut(hash) {
                    *count -= 1;
                    if *count == 0 {
                        to_evict.push(hash.clone());
                        refcounts.remove(hash);
                    }
                }
            }
            to_evict
        };

        if !to_evict.is_empty() {
            let result = self.backend.evict(to_evict.clone()).await;
            if !backend_ok(&result) {
                tracing::warn!(
                    status = result.status,
                    responded = result.responded,
                    count = to_evict.len(),
                    "LMCache evict failed during session_fork_end"
                );
                outcome.backend_degraded = true;
            } else {
                outcome.evicted_count = to_evict.len();
            }
        }
        Ok(outcome)
    }

    // Restore a session's state (used when recovering from SQLite). Does not touch refcounts;
    // callers are expected to invoke `rebuild_refcounts()` once all sessions have been restored.
    pub async fn restore_session(&self, session_id: &str, state: SessionKvState) {
        self.sessions
            .lock()
            .await
            .insert(session_id.to_string(), state);
    }

    // Rebuild the global refcount table from the in-memory session maps.
    //
    // Used after daemon restart: persisted maps are restored via `restore_session` but the
    // refcount table is in-memory only. This recomputes "each session, each unique hash once"
    // so shared chunks retain the correct refcount and `turn_end`/`session_close` will not
    // drop a chunk another session still references.
    pub async fn rebuild_refcounts(&self) {
        let mut new_refcounts: HashMap<String, u64> = HashMap::new();
        let sessions = self.sessions.lock().await;
        for state in sessions.values() {
            let mut seen_per_session: HashSet<String> = HashSet::new();
            for hash in state.map.chunk_hashes() {
                if seen_per_session.insert(hash.clone()) {
                    *new_refcounts.entry(hash).or_insert(0) += 1;
                }
            }
        }
        let count = new_refcounts.len();
        drop(sessions);
        *self.refcounts.lock().await = new_refcounts;
        tracing::info!(
            refcount_entries = count,
            "rebuilt global refcounts after restart"
        );
    }

    // Replace the global refcount table (used when restoring from persisted data).
    pub async fn set_refcounts(&self, refcounts: HashMap<String, u64>) {
        *self.refcounts.lock().await = refcounts;
    }

    pub async fn sessions_count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    // Debug snapshot: log every session's ordered chunk hashes and the global refcounts.
    // Only emitted when `config.trace_events` is true, to observe state after each event.
    pub async fn log_snapshot(&self) {
        if !self.config.trace_events {
            return;
        }
        {
            let sessions = self.sessions.lock().await;
            if sessions.is_empty() {
                tracing::info!("[trace] sessions: <empty>");
            }
            for (id, st) in sessions.iter() {
                tracing::info!(
                    session_id = %id,
                    turn_count = st.turn_count,
                    chunk_hashes = ?st.map.chunk_hashes(),
                    "[trace] session map"
                );
            }
        }
        let refcounts = self.refcounts.lock().await;
        tracing::info!(refcounts = ?*refcounts, "[trace] global refcounts");
    }
}

// Helper used by `on_snapshot_restore_from` to compute the hashes that need their
// refcount decremented (those in `old_hashes` but not in `new_set`).
fn old_unique_iter(old_hashes: &[String], new_set: &HashSet<&String>) -> Vec<String> {
    old_hashes
        .iter()
        .filter(|h| !new_set.contains(h))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KvCacheConfig;

    fn test_config() -> KvCacheConfig {
        KvCacheConfig {
            enabled: true,
            debug_enabled: false,
            trace_events: false,
            debug_dir: String::new(),
            backend_url: String::new(),
            turn_start_prefetch: false,
        }
    }

    #[tokio::test]
    async fn duplicate_hashes_in_one_session_do_not_inflate_refcount() {
        // Two sessions both reference A; the second session updates from [A] to [A, A].
        // The duplicate A inside session 2 must not bump refcount twice, otherwise
        // closing session 2 would over-decrement and evict A while session 1 still uses it.
        let mgr = KvCacheManager::new_noop(test_config());

        mgr.on_turn_end("s1", vec!["A".into()], None).await.unwrap();
        mgr.on_turn_end("s2", vec!["A".into()], None).await.unwrap();
        // s2 update [A] -> [A, A]: refcount for A must remain 2 (s1+s2 once each).
        mgr.on_turn_end("s2", vec!["A".into(), "A".into()], None)
            .await
            .unwrap();

        mgr.on_session_close("s2").await.unwrap();
        // A must still be tracked for s1.
        let map = mgr.session_map("s1").await.expect("s1 must still exist");
        assert_eq!(map.chunk_hashes(), vec!["A".to_string()]);

        let refcounts = mgr.refcounts.lock().await;
        assert_eq!(refcounts.get("A"), Some(&1));
    }

    #[tokio::test]
    async fn snapshot_restore_is_idempotent_for_active_session() {
        // Calling snapshot_restore twice on the same active session must not double the
        // refcount for any hash.
        let mgr = KvCacheManager::new_noop(test_config());

        mgr.on_snapshot_restore("s1", vec!["A".into(), "B".into()])
            .await
            .unwrap();
        mgr.on_snapshot_restore("s1", vec!["A".into(), "B".into()])
            .await
            .unwrap();

        let refcounts = mgr.refcounts.lock().await;
        assert_eq!(refcounts.get("A"), Some(&1), "A must be referenced once");
        assert_eq!(refcounts.get("B"), Some(&1), "B must be referenced once");
    }

    #[tokio::test]
    async fn snapshot_restore_diff_does_not_leak_or_evict_shared_chunks() {
        // s1 holds [A, B]. Restore s1 with [A, C]: A must keep refcount 1, B must be
        // evicted (refcount 0), C must be added.
        let mgr = KvCacheManager::new_noop(test_config());
        mgr.on_snapshot_restore("s1", vec!["A".into(), "B".into()])
            .await
            .unwrap();
        let outcome = mgr
            .on_snapshot_restore("s1", vec!["A".into(), "C".into()])
            .await
            .unwrap();
        assert_eq!(outcome.evicted_count, 1, "B should have been evicted");

        let refcounts = mgr.refcounts.lock().await;
        assert_eq!(refcounts.get("A"), Some(&1));
        assert_eq!(refcounts.get("B"), None, "B must be removed after eviction");
        assert_eq!(refcounts.get("C"), Some(&1));
    }

    #[tokio::test]
    async fn fork_end_releases_snapshot_taken_at_fork_time() {
        // Parent starts with [A]. Fork captures [A]. Parent's map changes to [B].
        // fork_end must release the snapshot's [A], not the parent's current [B].
        let mgr = KvCacheManager::new_noop(test_config());
        mgr.on_turn_end("parent", vec!["A".into()], None)
            .await
            .unwrap();
        let fork = mgr.on_session_fork("parent").await.unwrap();
        // After fork: refcount[A] = 2 (parent.map + fork snapshot).
        {
            let refcounts = mgr.refcounts.lock().await;
            assert_eq!(refcounts.get("A"), Some(&2), "A held by parent + fork");
        }
        // Parent's turn_end changes map to [B]. Parent's reference to A is dropped
        // (refcount goes 2→1, only the fork snapshot holds A now); B is added.
        mgr.on_turn_end("parent", vec!["B".into()], None)
            .await
            .unwrap();
        {
            let refcounts = mgr.refcounts.lock().await;
            assert_eq!(
                refcounts.get("A"),
                Some(&1),
                "A only held by the fork snapshot"
            );
            assert_eq!(refcounts.get("B"), Some(&1), "B held by parent");
        }
        // fork_end must decrement A (from the snapshot taken at fork time) and
        // NOT touch B (the parent's current map). Without the snapshot fix,
        // fork_end would read the current map [B] and wrongly evict B while
        // leaking A's fork reference forever.
        let outcome = mgr
            .on_session_fork_end("parent", Some(fork.fork_id))
            .await
            .unwrap();
        assert_eq!(
            outcome.evicted_count, 1,
            "fork_end should evict A (snapshot)"
        );
        {
            let refcounts = mgr.refcounts.lock().await;
            assert!(refcounts.get("A").is_none(), "A must be evicted");
            assert_eq!(refcounts.get("B"), Some(&1), "B untouched by fork_end");
        }
    }

    #[tokio::test]
    async fn duplicate_fork_end_is_idempotent() {
        // A second fork_end with the same fork_id must NOT decrement again.
        let mgr = KvCacheManager::new_noop(test_config());
        mgr.on_turn_end("parent", vec!["A".into()], None)
            .await
            .unwrap();
        let fork = mgr.on_session_fork("parent").await.unwrap();
        let _ = mgr
            .on_session_fork_end("parent", Some(fork.fork_id))
            .await
            .unwrap();
        let outcome = mgr
            .on_session_fork_end("parent", Some(fork.fork_id))
            .await
            .unwrap();
        assert_eq!(outcome.evicted_count, 0, "second fork_end must be a no-op");
        {
            let refcounts = mgr.refcounts.lock().await;
            assert_eq!(refcounts.get("A"), Some(&1), "parent's reference untouched");
        }
    }

    #[tokio::test]
    async fn rebuild_refcounts_after_restart_handles_shared_hashes() {
        // Simulate restart by manually inserting two sessions that share A.
        // rebuild_refcounts must produce refcount[A]=2 so closing one session does not
        // evict A while the other still uses it.
        let mgr = KvCacheManager::new_noop(test_config());
        mgr.restore_session(
            "s1",
            SessionKvState {
                map: KvCacheMap(vec!["A".into(), "B".into()]),
                turn_count: 1,
            },
        )
        .await;
        mgr.restore_session(
            "s2",
            SessionKvState {
                map: KvCacheMap(vec!["A".into(), "C".into()]),
                turn_count: 1,
            },
        )
        .await;
        // Pre-rebuild, refcount table is empty.
        {
            let refcounts = mgr.refcounts.lock().await;
            assert!(refcounts.is_empty());
        }
        mgr.rebuild_refcounts().await;
        {
            let refcounts = mgr.refcounts.lock().await;
            assert_eq!(
                refcounts.get("A"),
                Some(&2),
                "shared A counted once per session"
            );
            assert_eq!(refcounts.get("B"), Some(&1));
            assert_eq!(refcounts.get("C"), Some(&1));
        }
        // Closing s1 must not evict A (still referenced by s2).
        let outcome = mgr.on_session_close("s1").await.unwrap();
        assert_eq!(
            outcome.evicted_count, 1,
            "only B should be evicted (A still shared)"
        );
        {
            let refcounts = mgr.refcounts.lock().await;
            assert_eq!(refcounts.get("A"), Some(&1));
            assert_eq!(refcounts.get("C"), Some(&1));
            assert!(refcounts.get("B").is_none());
        }
    }

    #[tokio::test]
    async fn rebuild_refcounts_dedups_duplicate_hashes_within_a_session() {
        // A persisted session with [A, A] must count A only once after rebuild.
        let mgr = KvCacheManager::new_noop(test_config());
        mgr.restore_session(
            "s1",
            SessionKvState {
                map: KvCacheMap(vec!["A".into(), "A".into()]),
                turn_count: 0,
            },
        )
        .await;
        mgr.rebuild_refcounts().await;
        let refcounts = mgr.refcounts.lock().await;
        assert_eq!(refcounts.get("A"), Some(&1));
    }

    #[tokio::test]
    async fn snapshot_restore_from_reads_source_session_into_target() {
        // Plugin resume flow: a new session must inherit hashes from the previous one.
        let mgr = KvCacheManager::new_noop(test_config());
        // Persist previous session with [A, B].
        mgr.on_turn_end("prev", vec!["A".into(), "B".into()], None)
            .await
            .unwrap();
        // Now resume into a new session id, sourcing hashes from prev.
        let outcome = mgr
            .on_snapshot_restore_from("prev", "new", vec!["A".into(), "B".into()])
            .await
            .unwrap();
        assert!(outcome.prefetch_sent);
        // Both sessions reference A and B, so refcount must be 2 each.
        let refcounts = mgr.refcounts.lock().await;
        assert_eq!(refcounts.get("A"), Some(&2));
        assert_eq!(refcounts.get("B"), Some(&2));
    }

    #[tokio::test]
    async fn turn_end_increments_turn_count_and_persists() {
        // turn_count must be queryable for persistence so it is not hard-coded to 0.
        let mgr = KvCacheManager::new_noop(test_config());
        assert!(mgr.session_turn_count("s1").await.is_none());
        mgr.on_turn_end("s1", vec!["A".into()], None).await.unwrap();
        assert_eq!(mgr.session_turn_count("s1").await, Some(1));
        mgr.on_turn_end("s1", vec!["A".into(), "B".into()], None)
            .await
            .unwrap();
        assert_eq!(mgr.session_turn_count("s1").await, Some(2));
    }
}

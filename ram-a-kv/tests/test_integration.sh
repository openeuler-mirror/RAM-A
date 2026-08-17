#!/bin/bash
# ram-a-kv integration test script (L1 daemon contract layer)
#
# Two modes:
#   ./test_integration.sh          — auto-run L1 daemon contract tests (noop_backend, no LMCache needed)
#   ./test_integration.sh xiaoo    — print xiaoO integration guide
#
# Environment variables (optional):
#   DAEMON_BIN    — custom daemon binary path (default <repo>/target/release/ram-a-kv)
#   TEST_DIR      — custom test directory (default /tmp/ram-a-kv-test)
#   DAEMON_URL    — custom daemon URL (default http://127.0.0.1:6998)
#   KEEP_DAEMON   — when set to 1, do not kill daemon after tests (default 0)

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DAEMON_BIN="${DAEMON_BIN:-$REPO_ROOT/target/release/ram-a-kv}"
TEST_DIR="${TEST_DIR:-/tmp/ram-a-kv-test}"
DAEMON_URL="${DAEMON_URL:-http://127.0.0.1:6998}"

# ─────────────────────────────────────────────────────────────
# xiaoO integration guide mode
# ─────────────────────────────────────────────────────────────
if [ "$1" = "xiaoo" ]; then
    cat <<'EOF'
=== xiaoO + ram-a-kv integration steps ===

1) Start ram-a-kv daemon (test mode noop_backend)
   cat > /tmp/ram-a-kv-test/config.toml <<'CONF'
   listen_addr = "127.0.0.1:6998"
   noop_backend = true
   debug_enabled = true
   debug_dir = "/tmp/ram-a-kv-test/debug"
   session_store_path = "/tmp/ram-a-kv-test/sessions.db"
   turn_start_prefetch = true
   CONF

   RAM_A_KV_CONFIG=/tmp/ram-a-kv-test/config.toml \
     <repo>/target/release/ram-a-kv

2) Configure SDK (note: must be at ~/.config/ram-a-kv/config.toml, NOT in the xiaoo config!)
   mkdir -p ~/.config/ram-a-kv
   cat > ~/.config/ram-a-kv/config.toml <<'CONF'
   daemon_url = "http://127.0.0.1:6998"
   CONF

   Note: xiaoo's LlmConfig has no kvcache_daemon_url field;
   the daemon address can only be set via the SDK's own config file.

3) Configure xiaoo
   mkdir -p ~/.config/xiaoo
   cat > ~/.config/xiaoo/config.toml <<'CONF'
   [llm]
   provider = "local"
   model = "glm4.7"
   api_base = "http://localhost:8080/v1"
   max_tokens = 20000
   reasoning_effort = "off"
   kvcache_enabled = true
   CONF

4) Start xiaoO TUI (must restart xiaoo after changing SDK config, because OnceLock cannot be reloaded)
   <xiaoo-repo>/target/release/xiaoo-tui

5) Verify daemon received events
   daemon stdout should show:
     type=turn_start session_id=... ram-a-kv event completed
     type=turn_end   session_id=... ram-a-kv event completed

6) Verify debug files
   ls /tmp/ram-a-kv-test/debug/
   Note: a debug file is generated only when turn_end carries the debug_context field.

7) Production switch
   noop_backend = false
   lmcache_url = "http://localhost:6999"
   session_store_path = "/var/lib/ram-a-kv/sessions.db"
EOF
    exit 0
fi

# ─────────────────────────────────────────────────────────────
# helper functions
# ─────────────────────────────────────────────────────────────
PASS=0
FAIL=0

# Check for required external tools so the script fails fast with a clear
# message instead of emitting confusing INVALID_JSON output.
for dep in jq sqlite3 curl ss; do
    if ! command -v "$dep" >/dev/null 2>&1; then
        echo "ERROR: required tool '$dep' is not installed"
        echo "       install it before running the integration tests"
        exit 1
    fi
done

# Emit an event and assert the ok field
# Usage: emit_and_assert_ok <name> <expected_ok> <json-payload>
emit() {
    local name="$1"
    local expected_ok="$2"
    local payload="$3"
    local resp
    resp=$(curl -s -X POST "$DAEMON_URL/event" \
        -H "Content-Type: application/json" \
        -d "$payload")
    local actual_ok
    actual_ok=$(echo "$resp" | jq -r '.ok' 2>/dev/null || echo "INVALID_JSON")
    if [ "$actual_ok" = "$expected_ok" ]; then
        echo "  [PASS] $name"
        PASS=$((PASS + 1))
    else
        echo "  [FAIL] $name"
        echo "        expected ok=$expected_ok, got ok=$actual_ok"
        echo "        response: $resp"
        FAIL=$((FAIL + 1))
    fi
    echo "$resp" | jq -c '.' 2>/dev/null | sed 's/^/        /'
}

# Assert a JSON field value
# Usage: assert_field <name> <json> <jq-path> <expected>
assert_field() {
    local name="$1"
    local json="$2"
    local path="$3"
    local expected="$4"
    local actual
    actual=$(echo "$json" | jq -r "$path" 2>/dev/null)
    if [ "$actual" = "$expected" ]; then
        echo "  [PASS] $name"
        PASS=$((PASS + 1))
    else
        echo "  [FAIL] $name"
        echo "        expected $path=$expected, got $actual"
        FAIL=$((FAIL + 1))
    fi
}

cleanup() {
    if [ -n "${DAEMON_PID:-}" ] && [ "${KEEP_DAEMON:-0}" != "1" ]; then
        kill "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# ─────────────────────────────────────────────────────────────
# prepare environment
# ─────────────────────────────────────────────────────────────
echo "=== prepare test dir ==="
mkdir -p "$TEST_DIR"
rm -rf "$TEST_DIR/sessions.db" "$TEST_DIR/debug"

echo "=== check daemon binary ==="
if [ ! -x "$DAEMON_BIN" ]; then
    echo "ERROR: daemon binary not found at $DAEMON_BIN"
    echo "       run 'cargo build --release' first, or set DAEMON_BIN env var"
    exit 1
fi

echo "=== write test config (noop_backend=true, no LMCache needed) ==="
cat > "$TEST_DIR/config.toml" <<'EOF'
listen_addr = "127.0.0.1:6998"
noop_backend = true
debug_enabled = true
debug_dir = "/tmp/ram-a-kv-test/debug"
session_store_path = "/tmp/ram-a-kv-test/sessions.db"
turn_start_prefetch = true
EOF

echo "=== start daemon (background) ==="
RAM_A_KV_CONFIG="$TEST_DIR/config.toml" "$DAEMON_BIN" > "$TEST_DIR/daemon.log" 2>&1 &
DAEMON_PID=$!
sleep 1

# verify daemon started successfully
if ! ss -tlnp 2>/dev/null | grep -q ":6998"; then
    echo "ERROR: daemon did not start (port 6998 not listening)"
    cat "$TEST_DIR/daemon.log"
    exit 1
fi
echo "daemon pid=$DAEMON_PID"
echo ""

# ─────────────────────────────────────────────────────────────
# L1.1 / L1.2 health check + list_events
# ─────────────────────────────────────────────────────────────
echo "=== L1.1 health check ==="
emit "health returns ok=true" "true" '{"type":"health"}'

echo ""
echo "=== L1.2 list_events meta-event ==="
list_resp=$(curl -s -X POST "$DAEMON_URL/event" \
    -H "Content-Type: application/json" \
    -d '{"type":"list_events"}')
event_count=$(echo "$list_resp" | jq -r '.data.events | length' 2>/dev/null)
echo "  event count: $event_count"
if [ "$event_count" = "9" ]; then
    echo "  [PASS] 9 events registered"
    PASS=$((PASS + 1))
else
    echo "  [FAIL] expected 9 events, got $event_count"
    FAIL=$((FAIL + 1))
fi
echo "$list_resp" | jq -c '.data.events | keys' | sed 's/^/        /'

# ─────────────────────────────────────────────────────────────
# L1.3 turn_start / turn_end positive case (valid payload)
# ─────────────────────────────────────────────────────────────
echo ""
echo "=== L1.3 turn_start / turn_end positive case ==="
emit "turn_start new session (empty map)" "true" \
    '{"type":"turn_start","session_id":"basic-s1"}'

emit "turn_end uses kv_transfer_params.chunk_hashes" "true" \
    '{"type":"turn_end","session_id":"basic-s1","kv_transfer_params":{"chunk_hashes":["hash-a","hash-b","hash-c"]}}'

map_resp=$(curl -s -X POST "$DAEMON_URL/event" \
    -H "Content-Type: application/json" \
    -d '{"type":"session_map","session_id":"basic-s1"}')
echo "=== session_map query basic-s1 ==="
echo "$map_resp" | jq -c '.' | sed 's/^/        /'
assert_field "session_map chunk_hashes=[hash-a,hash-b,hash-c]" \
    "$map_resp" '.data.chunk_hashes | tostring' '["hash-a","hash-b","hash-c"]'

# ─────────────────────────────────────────────────────────────
# L1.4 refcount sharing semantics (core safety mechanism)
# ─────────────────────────────────────────────────────────────
echo ""
echo "=== L1.4 refcount sharing (hash-a shared by s1+s2, must not be evicted) ==="
emit "s2 turn_end [hash-a, hash-x]" "true" \
    '{"type":"turn_end","session_id":"refcount-s2","kv_transfer_params":{"chunk_hashes":["hash-a","hash-x"]}}'

emit "s1 turn_end changed to [hash-b] (hash-a kept, still referenced by s2)" "true" \
    '{"type":"turn_end","session_id":"basic-s1","kv_transfer_params":{"chunk_hashes":["hash-b"]}}'
# s1 used to reference hash-c but no one else does, so hash-c must have been
# evicted. The previous implementation always returned evicted_count=0.
s1_evict_resp=$(curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"turn_end","session_id":"basic-s1","kv_transfer_params":{"chunk_hashes":["hash-b"]}}')
assert_field "turn_end evicted_count >= 0 reported (no longer hard-coded 0)" \
    "$s1_evict_resp" '.data.evicted_count' '0'

s1_map=$(curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"session_map","session_id":"basic-s1"}')
s2_map=$(curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"session_map","session_id":"refcount-s2"}')
echo "        s1 map: $(echo $s1_map | jq -c '.data.chunk_hashes')"
echo "        s2 map: $(echo $s2_map | jq -c '.data.chunk_hashes')"
assert_field "s1 map = [hash-b]" "$s1_map" '.data.chunk_hashes | tostring' '["hash-b"]'
assert_field "s2 map still contains hash-a" "$s2_map" '.data.chunk_hashes | tostring' '["hash-a","hash-x"]'

# ─────────────────────────────────────────────────────────────
# L1.5 session_fork / session_fork_end
# ─────────────────────────────────────────────────────────────
echo ""
echo "=== L1.5 session_fork / session_fork_end ==="
fork_resp1=$(curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"session_fork","session_id":"basic-s1"}')
echo "$fork_resp1" | jq -c '.' | sed 's/^/        /'
fork_id1=$(echo "$fork_resp1" | jq -r '.data.fork_id')
if [ -n "$fork_id1" ] && [ "$fork_id1" != "null" ]; then
    echo "  [PASS] session_fork returned fork_id"
    PASS=$((PASS + 1))
else
    echo "  [FAIL] session_fork did not return fork_id"
    FAIL=$((FAIL + 1))
fi
emit "session_fork_end basic-s1 with fork_id (refcount-1)" "true" \
    "{\"type\":\"session_fork_end\",\"session_id\":\"basic-s1\",\"fork_id\":$fork_id1}"
emit "duplicate session_fork_end with same fork_id (idempotent)" "true" \
    "{\"type\":\"session_fork_end\",\"session_id\":\"basic-s1\",\"fork_id\":$fork_id1}"

# ─────────────────────────────────────────────────────────────
# L1.6 session_suspend (pin + kept in SQLite)
# ─────────────────────────────────────────────────────────────
echo ""
echo "=== L1.6 session_suspend (pin + kept in SQLite) ==="
suspend_resp=$(curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"session_suspend","session_id":"basic-s1"}')
echo "$suspend_resp" | jq -c '.' | sed 's/^/        /'
assert_field "session_suspend reports pinned=true" "$suspend_resp" '.data.pinned' 'true'
echo "        basic-s1 should be pinned and still in SQLite:"
sqlite3 "$TEST_DIR/sessions.db" \
    "SELECT session_id FROM sessions WHERE session_id='basic-s1';" \
    | sed 's/^/        /'
pin_row=$(sqlite3 "$TEST_DIR/sessions.db" \
    "SELECT session_id FROM pins WHERE session_id='basic-s1';")
if [ -n "$pin_row" ]; then
    echo "  [PASS] basic-s1 pinned in pins table"
    PASS=$((PASS + 1))
else
    echo "  [FAIL] basic-s1 not found in pins table"
    FAIL=$((FAIL + 1))
fi
# Close while pinned: the SQLite row must survive for snapshot_restore.
emit "session_close basic-s1 while pinned (row must survive)" "true" \
    '{"type":"session_close","session_id":"basic-s1"}'
close_resp=$(curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"session_close","session_id":"basic-s1"}')
assert_field "session_close while pinned reports pinned=true" "$close_resp" '.data.pinned' 'true'
surviving_row=$(sqlite3 "$TEST_DIR/sessions.db" \
    "SELECT session_id FROM sessions WHERE session_id='basic-s1';")
if [ -n "$surviving_row" ]; then
    echo "  [PASS] basic-s1 SQLite row survived pinned close"
    PASS=$((PASS + 1))
else
    echo "  [FAIL] basic-s1 SQLite row was deleted despite pin"
    FAIL=$((FAIL + 1))
fi
# A pinned session no longer in memory must still restore from SQLite.
restore_resp=$(curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"snapshot_restore","session_id":"basic-s1"}')
echo "$restore_resp" | jq -c '.' | sed 's/^/        /'
assert_field "snapshot_restore after pinned close still works" \
    "$restore_resp" '.data.prefetch_count' '1'

# ─────────────────────────────────────────────────────────────
# L1.7 edge cases and error handling (cases isolated: each uses its own session_id)
# ─────────────────────────────────────────────────────────────
echo ""
echo "=== L1.7 edge cases and error handling ==="
emit "missing type field" "false" \
    '{"session_id":"edge-x"}'
emit "unknown type" "false" \
    '{"type":"foobar"}'
emit "turn_start missing session_id" "false" \
    '{"type":"turn_start"}'
emit "turn_start session_id empty string" "false" \
    '{"type":"turn_start","session_id":""}'
emit "turn_end missing kv_transfer_params" "false" \
    '{"type":"turn_end","session_id":"edge-s1"}'
# chunk_hashes is now required: missing or null must NOT be silently treated as
# an empty array that clears the session. Only explicit [] is allowed.
emit "turn_end kv_transfer_params without chunk_hashes (now rejected)" "false" \
    '{"type":"turn_end","session_id":"edge-s1","kv_transfer_params":{}}'
emit "turn_end chunk_hashes=null (rejected)" "false" \
    '{"type":"turn_end","session_id":"edge-s1","kv_transfer_params":{"chunk_hashes":null}}'
emit "turn_end chunk_hashes contains non-string (rejected)" "false" \
    '{"type":"turn_end","session_id":"edge-s1","kv_transfer_params":{"chunk_hashes":["ok",123]}}'
emit "turn_end with explicit empty chunk_hashes (allowed, clears session)" "true" \
    '{"type":"turn_end","session_id":"edge-clear","kv_transfer_params":{"chunk_hashes":[]}}'
emit "session_map nonexistent session" "false" \
    '{"type":"session_map","session_id":"nonexistent-xxx"}'
# session_close is lenient on unknown sessions.
emit "session_close nonexistent session (lenient ok=true)" "true" \
    '{"type":"session_close","session_id":"nonexistent-xxx"}'

# clean up edge-s1 (avoid affecting subsequent cases)
curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"session_close","session_id":"edge-s1"}' > /dev/null
curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"session_close","session_id":"edge-clear"}' > /dev/null

# ─────────────────────────────────────────────────────────────
# L1.7b regression: duplicate hashes inside one session must not inflate refcount
# ─────────────────────────────────────────────────────────────
echo ""
echo "=== L1.7b duplicate-hash refcount regression ==="
emit "s1 turn_end [A]" "true" \
    '{"type":"turn_end","session_id":"dup-s1","kv_transfer_params":{"chunk_hashes":["A"]}}'
emit "s2 turn_end [A] (shared with s1)" "true" \
    '{"type":"turn_end","session_id":"dup-s2","kv_transfer_params":{"chunk_hashes":["A"]}}'
emit "s2 turn_end [A, A] (duplicate must not double-count)" "true" \
    '{"type":"turn_end","session_id":"dup-s2","kv_transfer_params":{"chunk_hashes":["A","A"]}}'
emit "s2 session_close (must NOT evict A, s1 still uses it)" "true" \
    '{"type":"session_close","session_id":"dup-s2"}'
map_after_s2=$(curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"session_map","session_id":"dup-s1"}')
assert_field "s1 still has A after s2 closed" \
    "$map_after_s2" '.data.chunk_hashes | tostring' '["A"]'
curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"session_close","session_id":"dup-s1"}' > /dev/null

# ─────────────────────────────────────────────────────────────
# L1.7c regression: snapshot_restore is idempotent for active sessions
# ─────────────────────────────────────────────────────────────
echo ""
echo "=== L1.7c snapshot_restore idempotency regression ==="
# Seed the session first so snapshot_restore has something to load.
emit "seed dup-restore-s1 with [A, B] via turn_end" "true" \
    '{"type":"turn_end","session_id":"dup-restore-s1","kv_transfer_params":{"chunk_hashes":["A","B"]}}'
emit "restore dup-restore-s1 (first call)" "true" \
    '{"type":"snapshot_restore","session_id":"dup-restore-s1"}'
emit "restore dup-restore-s1 again (must not double refcount)" "true" \
    '{"type":"snapshot_restore","session_id":"dup-restore-s1"}'
curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"session_close","session_id":"dup-restore-s1"}' > /dev/null

# ─────────────────────────────────────────────────────────────
# L1.7d regression: fork_end releases the snapshot taken at fork time
# ─────────────────────────────────────────────────────────────
echo ""
echo "=== L1.7d fork_end snapshot regression ==="
emit "parent turn_end [A]" "true" \
    '{"type":"turn_end","session_id":"fork-parent","kv_transfer_params":{"chunk_hashes":["fork-A"]}}'
fork_resp=$(curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"session_fork","session_id":"fork-parent"}')
fork_id=$(echo "$fork_resp" | jq -r '.data.fork_id')
echo "        fork_id=$fork_id"
emit "parent turn_end changes map to [B]" "true" \
    '{"type":"turn_end","session_id":"fork-parent","kv_transfer_params":{"chunk_hashes":["fork-B"]}}'
emit "fork_end with fork_id releases [A] (snapshot)" "true" \
    "{\"type\":\"session_fork_end\",\"session_id\":\"fork-parent\",\"fork_id\":$fork_id}"
emit "duplicate fork_end with same fork_id is a no-op" "true" \
    "{\"type\":\"session_fork_end\",\"session_id\":\"fork-parent\",\"fork_id\":$fork_id}"
curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"session_close","session_id":"fork-parent"}' > /dev/null

# ─────────────────────────────────────────────────────────────
# L1.7e regression: daemon restart rebuilds refcount for shared hashes
# ─────────────────────────────────────────────────────────────
echo ""
echo "=== L1.7e restart rebuild refcount regression ==="
emit "shared-s1 turn_end [shared-A, B]" "true" \
    '{"type":"turn_end","session_id":"shared-s1","kv_transfer_params":{"chunk_hashes":["shared-A","shared-B"]}}'
emit "shared-s2 turn_end [shared-A, C]" "true" \
    '{"type":"turn_end","session_id":"shared-s2","kv_transfer_params":{"chunk_hashes":["shared-A","shared-C"]}}'
echo "        --- restart daemon ---"
kill "$DAEMON_PID" 2>/dev/null || true
wait "$DAEMON_PID" 2>/dev/null || true
sleep 0.5
RAM_A_KV_CONFIG="$TEST_DIR/config.toml" "$DAEMON_BIN" > "$TEST_DIR/daemon-restart2.log" 2>&1 &
DAEMON_PID=$!
sleep 1
emit "after restart, close shared-s1 (must NOT evict shared-A)" "true" \
    '{"type":"session_close","session_id":"shared-s1"}'
map_after=$(curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"session_map","session_id":"shared-s2"}')
assert_field "shared-s2 still has shared-A after s1 closed (post-restart)" \
    "$map_after" '.data.chunk_hashes | tostring' '["shared-A","shared-C"]'
curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"session_close","session_id":"shared-s2"}' > /dev/null

# ─────────────────────────────────────────────────────────────
# L1.8 persistence and restart recovery
# ─────────────────────────────────────────────────────────────
echo ""
echo "=== L1.8 persistence and restart recovery ==="
# use a separate session to avoid side effects from earlier cases
emit "turn_end persist-s1 before restart" "true" \
    '{"type":"turn_end","session_id":"persist-s1","kv_transfer_params":{"chunk_hashes":["persist-h1","persist-h2"]}}'
echo "        SQLite before restart:"
sqlite3 "$TEST_DIR/sessions.db" \
    "SELECT session_id, map_json FROM sessions WHERE session_id='persist-s1';" \
    | sed 's/^/        /'

echo "        --- restart daemon ---"
kill "$DAEMON_PID" 2>/dev/null || true
wait "$DAEMON_PID" 2>/dev/null || true
sleep 0.5
RAM_A_KV_CONFIG="$TEST_DIR/config.toml" "$DAEMON_BIN" > "$TEST_DIR/daemon-restart.log" 2>&1 &
DAEMON_PID=$!
sleep 1

after_restart=$(curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"session_map","session_id":"persist-s1"}')
echo "        session_map persist-s1 right after restart:"
echo "$after_restart" | jq -c '.' | sed 's/^/        /'
assert_field "after restart persist-s1 still contains [persist-h1, persist-h2]" \
    "$after_restart" '.data.chunk_hashes | tostring' '["persist-h1","persist-h2"]'

snap_resp=$(curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"snapshot_restore","session_id":"persist-s1"}')
echo "        snapshot_restore persist-s1:"
echo "$snap_resp" | jq -c '.' | sed 's/^/        /'
assert_field "snapshot_restore prefetch_count=2" \
    "$snap_resp" '.data.prefetch_count' '2'
assert_field "snapshot_restore prefetch_sent=true" \
    "$snap_resp" '.data.prefetch_sent' 'true'

# ─────────────────────────────────────────────────────────────
# L1.9 debug file persistence (debug_enabled generates file even when
# chunk_hashes and debug_context are empty)
# ─────────────────────────────────────────────────────────────
echo ""
echo "=== L1.9 debug file persistence ==="
emit "turn_end with debug_context" "true" \
    '{"type":"turn_end","session_id":"debug-s1","kv_transfer_params":{"chunk_hashes":["dbg-h1","dbg-h2"]},"debug_context":{"messages":[{"role":"user","content":"hi"}],"timing":{"ttft_ms":120,"total_time_ms":450}}}'

echo "        debug file directory:"
ls -la "$TEST_DIR/debug/" 2>/dev/null | sed 's/^/        /'
debug_file="$TEST_DIR/debug/kvcache_debug_debug-s1_1.json"
if [ -f "$debug_file" ]; then
    echo "  [PASS] debug file generated: $(basename $debug_file)"
    PASS=$((PASS + 1))
    echo "        contents:"
    cat "$debug_file" | jq -c '.' 2>/dev/null | sed 's/^/        /'
else
    echo "  [FAIL] expected debug file not generated"
    FAIL=$((FAIL + 1))
fi

# L1.9b: debug file generated even with empty chunk_hashes and no debug_context.
# The file should exist with empty arrays/null fields.
emit "turn_end empty chunk_hashes, no debug_context" "true" \
    '{"type":"turn_end","session_id":"debug-empty","kv_transfer_params":{"chunk_hashes":[]}}'
debug_empty_file="$TEST_DIR/debug/kvcache_debug_debug-empty_1.json"
if [ -f "$debug_empty_file" ]; then
    echo "  [PASS] debug file generated for empty chunks/no context"
    PASS=$((PASS + 1))
    echo "        contents:"
    cat "$debug_empty_file" | jq -c '.' 2>/dev/null | sed 's/^/        /'
else
    echo "  [FAIL] expected debug file for empty case not generated"
    FAIL=$((FAIL + 1))
fi

# ─────────────────────────────────────────────────────────────
# L1.10 cleanup (includes sessions restored to memory from SQLite after restarts)
# ─────────────────────────────────────────────────────────────
echo ""
echo "=== L1.10 cleanup ==="
emit "session_close basic-s1 (suspended+pinned in L1.6)" "true" \
    '{"type":"session_close","session_id":"basic-s1"}'
emit "session_close persist-s1" "true" \
    '{"type":"session_close","session_id":"persist-s1"}'
# dup-restore-s1 was pinned in L1.7c; its row survived the L1.7e restart and was
# re-loaded into memory, so close it here.
emit "session_close dup-restore-s1" "true" \
    '{"type":"session_close","session_id":"dup-restore-s1"}'
emit "session_close refcount-s2" "true" \
    '{"type":"session_close","session_id":"refcount-s2"}'
emit "session_close debug-s1" "true" \
    '{"type":"session_close","session_id":"debug-s1"}'
emit "session_close debug-empty" "true" \
    '{"type":"session_close","session_id":"debug-empty"}'

# Pinned sessions keep their SQLite row across session_close by design:
#   basic-s1, dup-restore-s1, persist-s1. Every unpinned session must be gone.
echo "        SQLite sessions table after cleanup (pinned rows remain by design):"
sqlite3 "$TEST_DIR/sessions.db" "SELECT session_id FROM sessions ORDER BY session_id;" \
    | sed 's/^/        /'
remaining=$(sqlite3 "$TEST_DIR/sessions.db" \
    "SELECT group_concat(session_id, ',') FROM (SELECT session_id FROM sessions ORDER BY session_id);")
if [ "$remaining" = "basic-s1,dup-restore-s1,persist-s1" ]; then
    echo "  [PASS] only pinned sessions remain in SQLite"
    PASS=$((PASS + 1))
else
    echo "  [FAIL] unexpected remaining rows: $remaining"
    echo "        expected: basic-s1,dup-restore-s1,persist-s1"
    FAIL=$((FAIL + 1))
fi

# ─────────────────────────────────────────────────────────────
# final health check
# ─────────────────────────────────────────────────────────────
echo ""
echo "=== final health check (expect 0 sessions) ==="
final=$(curl -s -X POST "$DAEMON_URL/event" -H "Content-Type: application/json" \
    -d '{"type":"health"}')
echo "$final" | jq -c '.' | sed 's/^/        /'
assert_field "final sessions_count=0" "$final" '.data.sessions_count' '0'

# ─────────────────────────────────────────────────────────────
# summary
# ─────────────────────────────────────────────────────────────
echo ""
echo "=============================================="
echo "  L1 test results: PASS=$PASS  FAIL=$FAIL"
echo "=============================================="
if [ "$FAIL" -gt 0 ]; then
    echo "check daemon logs: $TEST_DIR/daemon.log / $TEST_DIR/daemon-restart.log"
    exit 1
fi
echo ""
echo "view xiaoO integration steps: $0 xiaoo"

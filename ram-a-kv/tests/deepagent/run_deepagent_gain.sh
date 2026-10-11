#!/bin/bash
# DeepAgents x RAM-A prefetch-gain experiment.
#
# Requires a python interpreter that can import deepagents / langchain_openai /
# ram_a_middleware (auto-detected; override with VENV_PY=/path/to/python).
#
# For each context size it runs the same DeepAgents conversation in two modes
# against a ram-a-kv daemon whose prefetch is toggled:
#     cold      near tier cold, daemon does NOT prefetch     (baseline)
#     prefetch  near tier cold, then daemon prefetches on turn_start
# and reports the per-turn latency difference (turn-level AND model-call-level).
# The near tier (DDR / FA buffer) is small and shared, so other requests evict a
# session's blocks to SSD; every cold/prefetch run replays the UCM pool to
# reproduce that evicted (cold) state deterministically.
#
# Usage
# -----
#   # single context, prompt from a file (long prompts must NOT go through argv)
#   PROMPT_FILE=my_prompt.txt tests/deepagent/run_deepagent_gain.sh
#
#   # single context, short prompt inline
#   tests/deepagent/run_deepagent_gain.sh "请总结预取如何降低首 token 时延?"
#
#   # sweep over several context sizes (tokens); prompts are generated
#   SWEEP=32768,65536,131072,262144 REPS=3 tests/deepagent/run_deepagent_gain.sh
#
# Environment overrides
# ---------------------
#   SWEEP            comma-separated context sizes in tokens -> sweep mode
#   PROMPT_FILE      starting prompt file (single mode)
#   MODEL_BASE_URL   OpenAI-compatible endpoint the agent talks to  (router http://127.0.0.1:9003/v1)
#   UCM_URL          UCM vllm-router base (daemon ucm_url)          (http://127.0.0.1:9003)
#   RAM_A_PORT       ram-a-kv daemon port                           (7098; avoid 6997-7004)
#   THREAD_ID        session id (single mode)                       (deepagent-gain)
#   REPS             repetitions of {cold, prefetch}                (3)
#   TURN_START_ASYNC 1 = async turn_start prefetch (recommended)    (1)
#   FLUSH            1 = flush near tier cold before each run        (1)
#   FLUSH_POOL       UCM flush pool json                            (/mnt/sdb/ucm/flush_pool_64g.json)
#   FLUSH_ENTRIES    pool entries replayed per flush                (16; must exceed ~1.2x FA size)
#   PROBE            probe question appended to the saved context   (固定中文探针)
#   HUMAN_TOKEN_LIMIT >0 时放宽单条消息淘汰阈值(测 >45k token 时需要) (0=默认50000)
#   MIN_SIZE         报告只统计 >= 该 token 数的尺寸(如 32768)      (0=全部)
#   VENV_PY          python with deepagents installed               (auto-detected)
#   DAEMON_BIN       ram-a-kv binary                                (<repo>/target/release/ram-a-kv)
#   KEEP_DAEMON      1 = leave the daemon running at the end         (0)
#   APPEND           1 = keep prior results.jsonl (incremental)      (0)
#   DATA_DIR         single output dir: results, report, RESULT.md, contexts,    (<script>/data)
#                    prompts, daemon log and sessions.db all land here

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$SCRIPT_DIR"

SWEEP="${SWEEP:-}"
PROMPT_FILE="${PROMPT_FILE:-}"
PROMPT="${1:-}"
PROBE="${PROBE:-请用一句话复述上文的核心要点。}"

MODEL_BASE_URL="${MODEL_BASE_URL:-http://127.0.0.1:9003/v1}"
UCM_URL="${UCM_URL:-http://127.0.0.1:9003}"
ROUTER_BASE="${ROUTER_BASE:-$UCM_URL}"
RAM_A_PORT="${RAM_A_PORT:-7098}"
DAEMON_URL="http://127.0.0.1:${RAM_A_PORT}"
REPS="${REPS:-3}"
FLUSH="${FLUSH:-1}"
FLUSH_POOL="${FLUSH_POOL:-/mnt/sdb/ucm/flush_pool_64g.json}"
# Must exceed the near-tier eviction threshold (~= 1.2 x FA size). With the engine
# started at UCM_CACHE_BUFFER_CAPACITY_GB=32 the FA side is ~16 GiB, so the
# threshold is ~19.2 GiB; a single pool group (16 x 1.6 GB = 25.6 GB) clears it
# with margin. 12 entries (19.2 GB) sits *exactly* on the threshold and evicts
# only non-deterministically -> the cold state becomes unstable.
FLUSH_ENTRIES="${FLUSH_ENTRIES:-16}"
# >0 raises FilesystemMiddleware's per-message eviction threshold so a single
# long prompt (needed for the >45k-token part of the sweep) is not turned into a
# file pointer. 0 keeps deepagents' default (50000).
HUMAN_TOKEN_LIMIT="${HUMAN_TOKEN_LIMIT:-0}"
# Report only sizes >= this many tokens (e.g. 32768 skips the noise-dominated
# small contexts). 0 = report every measured size.
MIN_SIZE="${MIN_SIZE:-0}"
DAEMON_BIN="${DAEMON_BIN:-$REPO_ROOT/target/release/ram-a-kv}"
KEEP_DAEMON="${KEEP_DAEMON:-0}"
ASYNC_FLAG="false"
# Default to async turn_start prefetch: it hides the router fan-out behind the
# model call and is what makes the prefetch benefit visible at the turn level.
[ "${TURN_START_ASYNC:-1}" = "1" ] && ASYNC_FLAG="true"

DATA_DIR="${DATA_DIR:-$SCRIPT_DIR/data}"
mkdir -p "$DATA_DIR"
RESULTS="$DATA_DIR/results.jsonl"
SUMMARY="$DATA_DIR/summary.json"
REPORT_MD="$DATA_DIR/report.md"
RESULT_MD="$DATA_DIR/RESULT.md"
ENV_FILE="$DATA_DIR/run_env.json"
# APPEND=1 keeps prior records (incremental runs / raising REPS later); default
# truncates so a fresh run cannot silently mix results from different setups.
[ "${APPEND:-0}" = "1" ] || rm -f "$RESULTS"

export NO_PROXY="127.0.0.1,localhost${NO_PROXY:+,$NO_PROXY}"
export no_proxy="$NO_PROXY"

# ── resolve a python that can import deepagents ────────────────────────────────
# Use this repo's middleware source (matches the daemon contract) when present.
# This must precede python detection: ram_a_middleware is provided by the repo,
# not installed into the interpreter.
if [ -d "$REPO_ROOT/deepagents-middleware/src" ]; then
    export PYTHONPATH="$REPO_ROOT/deepagents-middleware/src${PYTHONPATH:+:$PYTHONPATH}"
fi

detect_python() {
    local candidates=(
        "${VENV_PY:-}"
        "$REPO_ROOT/.venv/bin/python"
        "/opt/miniconda3/envs/ram-a-agent/bin/python"
        "$HOME/ram-a-sdk-main/.venv/bin/python"
        "python3"
    )
    for py in "${candidates[@]}"; do
        [ -n "$py" ] || continue
        if command -v "$py" >/dev/null 2>&1 && \
           "$py" -c 'import deepagents, langchain_openai, ram_a_middleware' >/dev/null 2>&1; then
            echo "$py"; return 0
        fi
    done
    return 1
}
PY="$(detect_python)" || {
    echo "ERROR: no python found that can 'import deepagents, langchain_openai, ram_a_middleware'." >&2
    echo "       install them (pip install '.[deepagents]') and/or pass VENV_PY=/path/to/python" >&2
    exit 1
}

# ── preflight ──────────────────────────────────────────────────────────────────
[ -x "$DAEMON_BIN" ] || { echo "ERROR: daemon binary not found: $DAEMON_BIN (run 'cargo build --release')" >&2; exit 1; }
if ! curl -sf -m5 -o /dev/null "$MODEL_BASE_URL/models"; then
    echo "ERROR: model endpoint not reachable: $MODEL_BASE_URL/models" >&2; exit 1
fi
echo "[setup] python=$PY"
echo "[setup] model_base_url=$MODEL_BASE_URL  ucm_url=$UCM_URL  daemon=$DAEMON_URL  async=$ASYNC_FLAG  reps=$REPS"

# ── daemon lifecycle ───────────────────────────────────────────────────────────
DAEMON_PID=""
write_config() {  # $1 = turn_start_prefetch (true|false)  $2 = turn_start_prefetch_async (true|false)
    local async="${2:-false}"
    cat > "$DATA_DIR/daemon.toml" <<EOF
listen_addr = "127.0.0.1:${RAM_A_PORT}"
noop_backend = false
ucm_backend = true
ucm_url = "${UCM_URL}"
session_store_path = "${DATA_DIR}/sessions.db"
turn_start_prefetch = $1
turn_start_prefetch_async = $async
debug_enabled = false
EOF
}
port_open() {  # 0 if something is listening on RAM_A_PORT (portable: no `ss` needed)
    (exec 3<>"/dev/tcp/127.0.0.1/${RAM_A_PORT}") 2>/dev/null
}
port_desc() { ss -ltnp 2>/dev/null | grep ":${RAM_A_PORT}" || echo "(port ${RAM_A_PORT} listener)"; }
daemon_start() {  # $1 = turn_start_prefetch (true|false)  $2 = turn_start_prefetch_async (true|false)
    # Refuse to start if the port is already taken, otherwise our daemon would
    # panic on bind while a *stale* daemon silently serves the measurements.
    if port_open; then
        echo "ERROR: port ${RAM_A_PORT} already in use; pick another RAM_A_PORT" >&2
        port_desc >&2
        exit 1
    fi
    write_config "$1" "${2:-false}"
    RAM_A_KV_CONFIG="$DATA_DIR/daemon.toml" "$DAEMON_BIN" > "$DATA_DIR/daemon.log" 2>&1 &
    DAEMON_PID=$!
    # The port was free an instant ago, so the first listener to appear is ours.
    for _ in $(seq 1 50); do
        if port_open; then return 0; fi
        if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
            echo "ERROR: daemon exited before binding :${RAM_A_PORT}" >&2
            cat "$DATA_DIR/daemon.log" >&2; exit 1
        fi
        sleep 0.1
    done
    echo "ERROR: daemon did not bind :${RAM_A_PORT}" >&2; cat "$DATA_DIR/daemon.log" >&2; exit 1
}
daemon_stop() {
    if [ -n "$DAEMON_PID" ]; then
        kill "$DAEMON_PID" 2>/dev/null || true
        for _ in $(seq 1 30); do kill -0 "$DAEMON_PID" 2>/dev/null || break; sleep 0.1; done
        kill -KILL "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
        DAEMON_PID=""
        # wait until the port is actually released before the next phase binds it
        for _ in $(seq 1 30); do port_open || break; sleep 0.1; done
    fi
}
cleanup() { [ "$KEEP_DAEMON" = "1" ] || daemon_stop; }
trap cleanup EXIT

# ── helpers ────────────────────────────────────────────────────────────────────
CUR_SIZE=0
CUR_REP=0
THREAD_ID="${THREAD_ID:-deepagent-gain}"
CONTEXT="$DATA_DIR/context_${THREAD_ID}.json"

py() { "$PY" "$SCRIPT_DIR/deepagent_gain.py" \
        --thread-id "$THREAD_ID" --context "$CONTEXT" --results "$RESULTS" --summary "$SUMMARY" \
        --report-md "$REPORT_MD" --result-md "$RESULT_MD" --env-file "$ENV_FILE" --probe "$PROBE" \
        --min-size "$MIN_SIZE" \
        --model-base-url "$MODEL_BASE_URL" --daemon-url "$DAEMON_URL" --router-base "$ROUTER_BASE" \
        --size "$CUR_SIZE" --rep "$CUR_REP" \
        --human-token-limit "$HUMAN_TOKEN_LIMIT" \
        --pool "$FLUSH_POOL" --flush-entries "$FLUSH_ENTRIES" "$@"; }

gen_prompt() {  # $1 = tokens, $2 = outfile
    "$PY" "$SCRIPT_DIR/gen_prompt.py" --tokens "$1" --out "$2"
}

run_one() {  # $1 mode  $2 turn_start_prefetch  $3 async  $4 flush(0|1)
    local mode="$1" pref="$2" async="$3" do_flush="$4" fflag=""
    echo ""
    echo "=== [measure] size=${CUR_SIZE} mode=${mode} (prefetch=${pref} async=${async} flush=${do_flush}) ==="
    daemon_start "$pref" "$async"
    [ "$do_flush" = "1" ] && fflag="--flush"
    py --phase measure --mode "$mode" $fflag
    daemon_stop
}

run_size() {  # $1 = tokens, $2 = prompt file, $3 = thread id, $4 = context file
    local size="$1" pfile="$2" thread="$3" ctx="$4"
    CUR_SIZE="$size"; THREAD_ID="$thread"; CONTEXT="$ctx"
    echo ""
    echo "########## context size = ${size} tokens ##########"
    echo "=== [warm] build the reusable session context ==="
    daemon_start true "$ASYNC_FLAG"
    if ! py --phase warm --prompt-file "$pfile"; then
        echo "WARNING: warm recorded no chunk_hashes for size ${size}; skipping its measurement." >&2
        daemon_stop
        return 0
    fi
    daemon_stop
    for rep in $(seq 1 "$REPS"); do
        CUR_REP="$rep"
        echo ""
        echo "########## size=${size} repetition ${rep}/${REPS} ##########"
        # Alternate the order across reps to cancel any first-run ordering bias.
        if [ $((rep % 2)) -eq 1 ]; then
            run_one "cold"     false false        "$FLUSH"
            run_one "prefetch" true  "$ASYNC_FLAG" "$FLUSH"
        else
            run_one "prefetch" true  "$ASYNC_FLAG" "$FLUSH"
            run_one "cold"     false false        "$FLUSH"
        fi
    done
}

write_env() {  # immutable snapshot of what produced the numbers (for the report)
    local git_rev
    git_rev="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
    cat > "$ENV_FILE" <<EOF
{
  "git_commit": "$git_rev",
  "model_base_url": "$MODEL_BASE_URL",
  "ucm_router": "$UCM_URL",
  "daemon_port": "$RAM_A_PORT",
  "turn_start_prefetch_async": "$ASYNC_FLAG",
  "flush_enabled": "$FLUSH",
  "flush_pool": "$FLUSH_POOL",
  "flush_entries": "$FLUSH_ENTRIES",
  "reps": "$REPS",
  "sizes_tokens": "$SWEEP",
  "max_tokens": "32",
  "human_token_limit": "$HUMAN_TOKEN_LIMIT",
  "probe": "$PROBE"
}
EOF
}

# ── main ───────────────────────────────────────────────────────────────────────
if [ -n "$SWEEP" ]; then
    write_env
    for size in ${SWEEP//,/ }; do
        pfile="$DATA_DIR/prompt_${size}.txt"
        gen_prompt "$size" "$pfile"
        run_size "$size" "$pfile" "deepagent-gain-${size}" "$DATA_DIR/context_deepagent-gain-${size}.json"
    done
else
    if [ -n "$PROMPT_FILE" ]; then
        [ -r "$PROMPT_FILE" ] || { echo "ERROR: cannot read PROMPT_FILE: $PROMPT_FILE" >&2; exit 2; }
        PROMPT="$(cat "$PROMPT_FILE")"
    fi
    if [ "$PROMPT" = "-" ]; then PROMPT="$(cat)"; fi
    if [ -z "$PROMPT" ]; then
        echo "usage: SWEEP=8192,16384,32768,40960 $0   # sweep generated prompts" >&2
        echo "   or: PROMPT_FILE=/path/prompt.txt $0   # single, long prompt" >&2
        echo "   or: $0 \"<short prompt>\"                 # single, inline prompt" >&2
        exit 2
    fi
    PROMPT_PATH="$DATA_DIR/prompt_${THREAD_ID}.txt"
    printf '%s' "$PROMPT" > "$PROMPT_PATH"
    CUR_SIZE="${SIZE:-0}"
    # crude token estimate so the report can still group the single-size run
    [ "$CUR_SIZE" = "0" ] && CUR_SIZE=$(python3 -c "import sys;print(round(len(open(sys.argv[1]).read())/4.4))" "$PROMPT_PATH")
    SWEEP="$CUR_SIZE"
    write_env
    run_size "$CUR_SIZE" "$PROMPT_PATH" "$THREAD_ID" "$CONTEXT"
fi

# ── report ─────────────────────────────────────────────────────────────────────
echo ""
echo "=== [report] prefetch vs cold baseline ==="
py --phase report
echo ""
echo "=== output (all under $DATA_DIR) ==="
echo "RESULT (看这里): $RESULT_MD"
echo "raw records    : $RESULTS"
echo "summary json   : $SUMMARY"
echo "report md      : $REPORT_MD"
echo "env snapshot   : $ENV_FILE"

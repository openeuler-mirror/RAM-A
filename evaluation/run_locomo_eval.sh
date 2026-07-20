#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
PROJECT_ROOT="$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)"
cd "$SCRIPT_DIR"

if [ -f .env ]; then
    set -a
    . ./.env
    set +a
fi

DATASET="${DATASET:-fixtures/locomo_sample.json}"
TOP_K="${TOP_K:-30}"
RESUME="${RESUME:-0}"
ANSWER_MODEL="${MODEL:-gpt-4o-mini}"
JUDGE_MODEL="${JUDGE_MODEL:-${MODEL:-gpt-4o-mini}}"
LLM_API_KEY_ENV="${LLM_API_KEY_ENV:-OPENAI_API_KEY}"
LLM_BASE_URL="${LLM_BASE_URL:-${OPENAI_BASE_URL:-${OPENAI_API_BASE:-}}}"
LLM_THINKING="${LLM_THINKING:-default}"
RERANK="${RERANK:-0}"
RERANK_PROVIDER="${RERANK_PROVIDER:-openrouter}"
RERANK_MODEL="${RERANK_MODEL:-cohere/rerank-v3.5}"
RERANK_API_KEY_ENV="${RERANK_API_KEY_ENV:-OPENROUTER_API_KEY}"
RERANK_BASE_URL="${RERANK_BASE_URL:-https://openrouter.ai/api/v1}"
RERANK_INPUT_K="${RERANK_INPUT_K:-40}"
RERANK_TIMEOUT_MS="${RERANK_TIMEOUT_MS:-}"
RERANK_FAIL_OPEN="${RERANK_FAIL_OPEN:-0}"
MEMORY_BENCH_GRAPH="${MEMORY_BENCH_GRAPH:-0}"
MEMORY_BENCH_SEARCH_MODE="${MEMORY_BENCH_SEARCH_MODE:-hybrid}"
GRAPH_WEIGHT="${GRAPH_WEIGHT:-0.2}"
GRAPH_FAIL_OPEN="${GRAPH_FAIL_OPEN:-0}"
GRAPH_MEMORY_SPACE_MODE="${GRAPH_MEMORY_SPACE_MODE:-auto}"
GRAPH_MEMORY_SPACE_FIELD="${GRAPH_MEMORY_SPACE_FIELD:-scope_id}"
GRAPH_OWNER_ID="${GRAPH_OWNER_ID:-benchmark}"
GRAPH_LLM_API_KEY_ENV="${GRAPH_LLM_API_KEY_ENV:-OPENROUTER_API_KEY}"
GRAPH_LLM_MODEL="${GRAPH_LLM_MODEL:-openai/gpt-4o-mini}"
GRAPH_LLM_BASE_URL="${GRAPH_LLM_BASE_URL:-https://openrouter.ai/api/v1}"
GRAPH_LLM_TIMEOUT_MS="${GRAPH_LLM_TIMEOUT_MS:-60000}"
RUN_ID="${RUN_ID:-$(date +%Y-%m-%dT%H%M%S)}"
if [ "${RUN_DIR:-}" ]; then
    case "$RUN_DIR" in
        /*) ;;
        *) RUN_DIR="${PROJECT_ROOT}/${RUN_DIR}" ;;
    esac
else
    RUN_DIR="${PROJECT_ROOT}/outputs/locomo/${RUN_ID}"
fi
MEM0_DIR="${RUN_DIR}/mem0"
MEMORY_BENCH_DIR="${RUN_DIR}/ram-a"
MEM0_STAGE_REPORT_DIR="${MEM0_DIR}/stage_reports"
MEMORY_BENCH_STAGE_REPORT_DIR="${MEMORY_BENCH_DIR}/stage_reports"

MEM0_STORAGE="${MEM0_DIR}/storage"
MEM0_RETRIEVAL="${MEM0_DIR}/search_results.json"
MEM0_RETRIEVAL_METRICS="${MEM0_DIR}/retrieval_metrics.json"
MEM0_RETRIEVAL_REPORT="${MEM0_STAGE_REPORT_DIR}/retrieval_report.html"
MEM0_ANSWERS="${MEM0_DIR}/responses.json"
MEM0_SCORES="${MEM0_DIR}/judge_results.json"
MEM0_METRICS="${MEM0_DIR}/qa_metrics.json"
MEM0_REPORT="${MEM0_STAGE_REPORT_DIR}/qa_report.html"
MEM0_MAIN_REPORT="${MEM0_DIR}/report.html"
MEM0_ERROR_REPORT="${MEM0_DIR}/errors.html"

MEMORY_BENCH_STORE="${MEMORY_BENCH_DIR}/store.sqlite"
MEMORY_BENCH_DATASET="${MEMORY_BENCH_DIR}/prepared.json"
MEMORY_BENCH_RETRIEVAL="${MEMORY_BENCH_DIR}/search_results.json"
MEMORY_BENCH_RETRIEVAL_METRICS="${MEMORY_BENCH_DIR}/retrieval_metrics.json"
MEMORY_BENCH_RETRIEVAL_REPORT="${MEMORY_BENCH_STAGE_REPORT_DIR}/retrieval_report.html"
MEMORY_BENCH_ANSWERS="${MEMORY_BENCH_DIR}/responses.json"
MEMORY_BENCH_SCORES="${MEMORY_BENCH_DIR}/judge_results.json"
MEMORY_BENCH_METRICS="${MEMORY_BENCH_DIR}/qa_metrics.json"
MEMORY_BENCH_REPORT="${MEMORY_BENCH_STAGE_REPORT_DIR}/qa_report.html"
MEMORY_BENCH_MAIN_REPORT="${MEMORY_BENCH_DIR}/report.html"
MEMORY_BENCH_ERROR_REPORT="${MEMORY_BENCH_DIR}/errors.html"

mkdir -p "$MEM0_DIR" "$MEMORY_BENCH_DIR" "$MEM0_STAGE_REPORT_DIR" "$MEMORY_BENCH_STAGE_REPORT_DIR"

MEMORY_BENCH_RERANK_ARGS=""
case "$RERANK" in
    1|true|TRUE|yes|YES)
        MEMORY_BENCH_RERANK_ARGS="--rerank --rerank-provider $RERANK_PROVIDER --rerank-model $RERANK_MODEL --rerank-api-key-env $RERANK_API_KEY_ENV --rerank-base-url $RERANK_BASE_URL --rerank-input-k $RERANK_INPUT_K"
        if [ -n "$RERANK_TIMEOUT_MS" ]; then
            MEMORY_BENCH_RERANK_ARGS="$MEMORY_BENCH_RERANK_ARGS --rerank-timeout-ms $RERANK_TIMEOUT_MS"
        fi
        case "$RERANK_FAIL_OPEN" in
            1|true|TRUE|yes|YES)
                MEMORY_BENCH_RERANK_ARGS="$MEMORY_BENCH_RERANK_ARGS --rerank-fail-open"
                ;;
        esac
        ;;
esac

MEMORY_BENCH_ADD_RESUME_ARGS=""
case "$RESUME" in
    1|true|TRUE|yes|YES)
        MEMORY_BENCH_ADD_RESUME_ARGS="--resume"
        ;;
esac

MEMORY_BENCH_GRAPH_ADD_ARGS=""
MEMORY_BENCH_GRAPH_SEARCH_ARGS=""
case "$MEMORY_BENCH_GRAPH" in
    1|true|TRUE|yes|YES)
        MEMORY_BENCH_GRAPH_COMMON_ARGS="--graph-weight $GRAPH_WEIGHT --graph-memory-space-mode $GRAPH_MEMORY_SPACE_MODE --graph-memory-space-field $GRAPH_MEMORY_SPACE_FIELD --graph-owner-id $GRAPH_OWNER_ID --graph-llm-api-key-env $GRAPH_LLM_API_KEY_ENV --graph-llm-model $GRAPH_LLM_MODEL --graph-llm-base-url $GRAPH_LLM_BASE_URL --graph-llm-timeout-ms $GRAPH_LLM_TIMEOUT_MS"
        MEMORY_BENCH_GRAPH_ADD_ARGS="--graph-build $MEMORY_BENCH_GRAPH_COMMON_ARGS"
        MEMORY_BENCH_GRAPH_SEARCH_ARGS="--graph $MEMORY_BENCH_GRAPH_COMMON_ARGS"
        case "$GRAPH_FAIL_OPEN" in
            1|true|TRUE|yes|YES)
                MEMORY_BENCH_GRAPH_SEARCH_ARGS="$MEMORY_BENCH_GRAPH_SEARCH_ARGS --graph-fail-open"
                ;;
        esac
        ;;
esac

stage_start() {
    STAGE_STARTED="$(date +%s)"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] [stage $1/$2] $3 started"
}

stage_done() {
    elapsed="$(($(date +%s) - STAGE_STARTED))"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] [stage $1/$2] $3 done | elapsed=${elapsed}s"
}

score_results() {
    result_name="$1"
    answer_file="$2"
    score_file="$3"
    metrics_file="$4"
    report_file="$5"
    stage_start 5 7 "${result_name} judge"
    if [ -n "$LLM_BASE_URL" ]; then
        python3 locomo/locomo_eval.py \
            --input "$answer_file" \
            --output "$score_file" \
            --judge-model "$JUDGE_MODEL" \
            --llm-api-key-env "$LLM_API_KEY_ENV" \
            --llm-base-url "$LLM_BASE_URL" \
            --llm-thinking "$LLM_THINKING"
    else
        python3 locomo/locomo_eval.py \
            --input "$answer_file" \
            --output "$score_file" \
            --judge-model "$JUDGE_MODEL" \
            --llm-api-key-env "$LLM_API_KEY_ENV" \
            --llm-thinking "$LLM_THINKING"
    fi
    stage_done 5 7 "${result_name} judge"
    stage_start 6 7 "${result_name} metrics"
    python3 locomo/locomo_metric.py \
        --input "$score_file" \
        --output-json "$metrics_file" \
        --html-report "$report_file"
    stage_done 6 7 "${result_name} metrics"
}

run_locomo_responses() {
    case "$LLM_API_KEY_ENV" in
        ''|*[!A-Za-z0-9_]*)
            echo "invalid LLM_API_KEY_ENV: $LLM_API_KEY_ENV" >&2
            exit 1
            ;;
    esac
    response_api_key="$(printenv "$LLM_API_KEY_ENV" || true)"
    if [ -z "$response_api_key" ]; then
        echo "missing answer API key: set $LLM_API_KEY_ENV or choose LLM_API_KEY_ENV" >&2
        exit 1
    fi
    if [ -n "$LLM_BASE_URL" ]; then
        OPENAI_API_KEY="$response_api_key" OPENAI_BASE_URL="$LLM_BASE_URL" MODEL="$ANSWER_MODEL" \
            python3 locomo/locomo_responses.py "$@"
    else
        OPENAI_API_KEY="$response_api_key" MODEL="$ANSWER_MODEL" \
            python3 locomo/locomo_responses.py "$@"
    fi
}

write_meta() {
    backend="$1"
    phase="$2"
    output_file="$3"
    backend_dir="$4"
    python3 locomo/write_run_meta.py \
        --output "$output_file" \
        --dataset "$DATASET" \
        --backend "$backend" \
        --phase "$phase" \
        --top-k "$TOP_K" \
        --run-dir "$backend_dir"
}

write_combined_report() {
    backend_dir="$1"
    retrieval_metrics="$2"
    qa_metrics="$3"
    main_report="$4"
    error_report="$5"
    python3 locomo/locomo_report.py \
        --retrieval-json "$retrieval_metrics" \
        --qa-json "$qa_metrics" \
        --run-meta "${backend_dir}/run_meta.json" \
        --output "$main_report" \
        --errors-output "$error_report"
}

run_mem0() {
    rm -rf "$MEM0_RETRIEVAL" "$MEM0_RETRIEVAL_METRICS" "$MEM0_RETRIEVAL_REPORT" "$MEM0_ANSWERS" "$MEM0_SCORES" "$MEM0_METRICS" "$MEM0_REPORT"

    stage_start 1 7 "mem0 add"
    python3 locomo/locomo_experiments.py \
        --technique-type mem0 --method add --dataset "$DATASET" \
        --storage-dir "$MEM0_STORAGE" --debug
    stage_done 1 7 "mem0 add"

    stage_start 2 7 "mem0 search"
    python3 locomo/locomo_experiments.py \
        --technique-type mem0 --method search --dataset "$DATASET" \
        --storage-dir "$MEM0_STORAGE" --top-k "$TOP_K" --output "$MEM0_RETRIEVAL"
    stage_done 2 7 "mem0 search"

    stage_start 3 7 "mem0 retrieval"
    python3 locomo/locomo_retrieval.py \
        --dataset "$DATASET" \
        --input "$MEM0_RETRIEVAL" \
        --input-format mem0 \
        --output-json "$MEM0_RETRIEVAL_METRICS" \
        --html-report "$MEM0_RETRIEVAL_REPORT"
    stage_done 3 7 "mem0 retrieval"

    stage_start 4 7 "mem0 answer"
    run_locomo_responses \
        --technique-type mem0 --input "$MEM0_RETRIEVAL" \
        --output "$MEM0_ANSWERS"
    stage_done 4 7 "mem0 answer"

    score_results "mem0" "$MEM0_ANSWERS" "$MEM0_SCORES" "$MEM0_METRICS" "$MEM0_REPORT"
    write_meta "mem0" "all" "${MEM0_DIR}/run_meta.json" "$MEM0_DIR"
    stage_start 7 7 "mem0 report"
    write_combined_report "$MEM0_DIR" "$MEM0_RETRIEVAL_METRICS" "$MEM0_METRICS" "$MEM0_MAIN_REPORT" "$MEM0_ERROR_REPORT"
    stage_done 7 7 "mem0 report"
}

run_memory_bench() {
    if [ -z "$MEMORY_BENCH_ADD_RESUME_ARGS" ]; then
        rm -f "$MEMORY_BENCH_STORE"
    fi
    rm -f "$MEMORY_BENCH_RETRIEVAL" "$MEMORY_BENCH_RETRIEVAL_METRICS" "$MEMORY_BENCH_RETRIEVAL_REPORT" "$MEMORY_BENCH_ANSWERS" "$MEMORY_BENCH_SCORES" "$MEMORY_BENCH_METRICS" "$MEMORY_BENCH_REPORT"

    python3 locomo/prepare_memory_bench.py \
        --dataset "$DATASET" \
        --output "$MEMORY_BENCH_DATASET"

    stage_start 1 7 "RAM-A add"
    cargo run --quiet --manifest-path ../Cargo.toml -p memory-bench -- \
        --store "$MEMORY_BENCH_STORE" \
        --store-backend sqlite \
        --search-mode hybrid \
        $MEMORY_BENCH_GRAPH_ADD_ARGS \
        add --dataset "$MEMORY_BENCH_DATASET" $MEMORY_BENCH_ADD_RESUME_ARGS
    stage_done 1 7 "RAM-A add"

    stage_start 2 7 "RAM-A search"
    cargo run --quiet --manifest-path ../Cargo.toml -p memory-bench -- \
        --store "$MEMORY_BENCH_STORE" \
        --store-backend sqlite \
        --search-mode "$MEMORY_BENCH_SEARCH_MODE" \
        $MEMORY_BENCH_RERANK_ARGS \
        $MEMORY_BENCH_GRAPH_SEARCH_ARGS \
        search --dataset "$MEMORY_BENCH_DATASET" --top-k "$TOP_K" \
        --output "$MEMORY_BENCH_RETRIEVAL"
    stage_done 2 7 "RAM-A search"

    stage_start 3 7 "RAM-A retrieval"
    python3 locomo/locomo_retrieval.py \
        --dataset "$DATASET" \
        --input "$MEMORY_BENCH_RETRIEVAL" \
        --output-json "$MEMORY_BENCH_RETRIEVAL_METRICS" \
        --html-report "$MEMORY_BENCH_RETRIEVAL_REPORT"
    stage_done 3 7 "RAM-A retrieval"

    stage_start 4 7 "RAM-A answer"
    run_locomo_responses \
        --technique-type memory_bench --dataset "$DATASET" --input "$MEMORY_BENCH_RETRIEVAL" \
        --output "$MEMORY_BENCH_ANSWERS"
    stage_done 4 7 "RAM-A answer"

    score_results "memory-bench" "$MEMORY_BENCH_ANSWERS" "$MEMORY_BENCH_SCORES" "$MEMORY_BENCH_METRICS" "$MEMORY_BENCH_REPORT"
    write_meta "RAM-A" "all" "${MEMORY_BENCH_DIR}/run_meta.json" "$MEMORY_BENCH_DIR"
    stage_start 7 7 "RAM-A report"
    write_combined_report "$MEMORY_BENCH_DIR" "$MEMORY_BENCH_RETRIEVAL_METRICS" "$MEMORY_BENCH_METRICS" "$MEMORY_BENCH_MAIN_REPORT" "$MEMORY_BENCH_ERROR_REPORT"
    stage_done 7 7 "RAM-A report"
}

case "${1:-}" in
    mem0)
        run_mem0
        ;;
    memory_bench)
        run_memory_bench
        ;;
    *)
        echo "Usage: $0 {mem0|memory_bench}" >&2
        exit 2
        ;;
esac

echo "[done] LoCoMo evaluation completed successfully."

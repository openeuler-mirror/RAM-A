#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
cd "${REPO_ROOT}"

SIZE="32k"
TOP_K="20"
ANSWER_MODEL="openai/gpt-4o-mini"
CONTEXT_TOKEN_BUDGET="2000"
EMBEDDING_MODEL="baai/bge-m3"
EMBEDDING_DIMS="1024"
API_KEY_ENV="OPENROUTER_API_KEY"
BASE_URL="https://openrouter.ai/api/v1"
RUN_DIR=""
WORK_DIR=""
COLLECTION_NAME=""
BACKEND="mem0"
SKIP_PREPARE="0"
SKIP_INGEST="0"
SKIP_ANSWER="0"
RESUME="0"

usage() {
  cat <<'EOF'
Usage: evaluation/scripts/run_personalmem_mem0_local_v1.sh [options]

Options:
  --size SIZE
  --top-k K
  --answer-model MODEL
  --context-token-budget N
  --embedding-model MODEL
  --embedding-dims DIMS
  --api-key-env ENV_NAME
  --base-url URL
  --run-dir DIR
  --work-dir DIR
  --collection-name NAME
  --skip-prepare
  --skip-ingest
  --skip-answer
  --resume
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --size) SIZE="$2"; shift 2 ;;
    --top-k) TOP_K="$2"; shift 2 ;;
    --answer-model) ANSWER_MODEL="$2"; shift 2 ;;
    --context-token-budget) CONTEXT_TOKEN_BUDGET="$2"; shift 2 ;;
    --embedding-model) EMBEDDING_MODEL="$2"; shift 2 ;;
    --embedding-dims) EMBEDDING_DIMS="$2"; shift 2 ;;
    --api-key-env) API_KEY_ENV="$2"; shift 2 ;;
    --base-url) BASE_URL="$2"; shift 2 ;;
    --run-dir) RUN_DIR="$2"; shift 2 ;;
    --work-dir) WORK_DIR="$2"; shift 2 ;;
    --collection-name) COLLECTION_NAME="$2"; shift 2 ;;
    --skip-prepare) SKIP_PREPARE="1"; shift ;;
    --skip-ingest) SKIP_INGEST="1"; shift ;;
    --skip-answer) SKIP_ANSWER="1"; shift ;;
    --resume) RESUME="1"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

embedding_tag="${EMBEDDING_MODEL##*/}"
embedding_tag="${embedding_tag//:/-}"
answer_model_tag="${ANSWER_MODEL##*/}"
answer_model_tag="${answer_model_tag//:/-}"
context_tag="ctx${CONTEXT_TOKEN_BUDGET}"
if [[ "${CONTEXT_TOKEN_BUDGET}" == "0" ]]; then
  context_tag="ctxfull"
fi
backend_tag="mem0"

if [[ -z "${RUN_DIR}" ]]; then
  RUN_DIR="outputs/personalmem/personalmem_${SIZE}_v1_${backend_tag}_top${TOP_K}_${context_tag}_${answer_model_tag}"
fi
if [[ -z "${WORK_DIR}" ]]; then
  WORK_DIR="${RUN_DIR}/mem0_local"
fi
if [[ -z "${COLLECTION_NAME}" ]]; then
  COLLECTION_NAME="personalmem_${SIZE}_mem0"
fi

PREPARED="data/personalmem/prepared/personalmem_${SIZE}_v1.json"
SEARCH_RESULTS="${RUN_DIR}/search_results.json"
RETRIEVAL_METRICS="${RUN_DIR}/retrieval_metrics.json"
RESPONSES="${RUN_DIR}/responses.json"
GRADES="${RUN_DIR}/grade_metrics.json"
CSV="${RUN_DIR}/grade_results.csv"

log() {
  printf '\n[%s] %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*"
}

run_step() {
  log "START: $*"
  "$@"
  log "DONE: $*"
}

require_file() {
  if [[ ! -f "$1" ]]; then
    echo "Missing required file: $1" >&2
    exit 1
  fi
}

require_dir() {
  if [[ ! -d "$1" ]]; then
    echo "Missing required directory: $1" >&2
    exit 1
  fi
}

print_grade_summary() {
  python3 - "${GRADES}" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    data = json.load(handle)

summary = data.get("summary", {})
print("\nPersonaMem grade summary")
for key in (
    "total",
    "valid_predictions",
    "correct",
    "answer_acc",
    "valid_answer_acc",
    "api_error_count",
    "parse_error_count",
    "avg_retrieved_contexts",
    "avg_context_tokens",
):
    print(f"{key}: {summary.get(key)}")
PY
}

log "PersonaMem mem0 local v1 pipeline"
log "size=${SIZE} top_k=${TOP_K} prepared=${PREPARED}"
log "answer_model=${ANSWER_MODEL} context_token_budget=${CONTEXT_TOKEN_BUDGET}"
log "run_dir=${RUN_DIR}"
log "work_dir=${WORK_DIR}"
log "collection_name=${COLLECTION_NAME}"
log "search_results=${SEARCH_RESULTS}"
log "retrieval_metrics=${RETRIEVAL_METRICS}"
log "responses=${RESPONSES}"
log "grades=${GRADES}"

if [[ "${SKIP_PREPARE}" != "1" ]]; then
  run_step python3 evaluation/personalmem/run.py download --size "${SIZE}"
  run_step python3 evaluation/personalmem/run.py prepare \
    --size "${SIZE}" \
    --schema-version benchmark-prepared-v1 \
    --prepared-dataset "${PREPARED}"
else
  log "SKIP: prepare"
fi
require_file "${PREPARED}"
mkdir -p "${RUN_DIR}"

if [[ "${SKIP_INGEST}" != "1" ]]; then
  ingest_args=(
    python3 evaluation/personalmem/run_mem0_local.py ingest
    --dataset "${PREPARED}"
    --work-dir "${WORK_DIR}"
    --collection-name "${COLLECTION_NAME}"
    --embedding-model "${EMBEDDING_MODEL}"
    --embedding-dims "${EMBEDDING_DIMS}"
    --api-key-env "${API_KEY_ENV}"
    --base-url "${BASE_URL}"
    --max-retries 8
    --retry-backoff-seconds 5
  )
  if [[ "${RESUME}" == "1" ]]; then
    ingest_args+=(--resume)
  else
    ingest_args+=(--rebuild)
  fi
  run_step "${ingest_args[@]}"
else
  log "SKIP: ingest"
  require_dir "${WORK_DIR}"
fi
require_dir "${WORK_DIR}"

run_step python3 evaluation/personalmem/run_mem0_local.py search \
  --dataset "${PREPARED}" \
  --work-dir "${WORK_DIR}" \
  --collection-name "${COLLECTION_NAME}" \
  --embedding-model "${EMBEDDING_MODEL}" \
  --embedding-dims "${EMBEDDING_DIMS}" \
  --api-key-env "${API_KEY_ENV}" \
  --base-url "${BASE_URL}" \
  --output "${SEARCH_RESULTS}" \
  --top-k "${TOP_K}" \
  --max-retries 8 \
  --retry-backoff-seconds 5

run_step python3 evaluation/personalmem/run.py eval \
  --dataset "${PREPARED}" \
  --store "${WORK_DIR}" \
  --embedding openrouter \
  --model "${EMBEDDING_MODEL}" \
  --dimensions "${EMBEDDING_DIMS}" \
  --output "${SEARCH_RESULTS}" \
  --report "${RETRIEVAL_METRICS}" \
  --run-dir "${RUN_DIR}" \
  --backend "${BACKEND}" \
  --top-k "${TOP_K}"

if [[ "${SKIP_ANSWER}" != "1" ]]; then
  answer_args=(
    python3 evaluation/personalmem/run.py answer
    --dataset "${PREPARED}"
    --store "${WORK_DIR}"
    --output "${SEARCH_RESULTS}"
    --report "${RETRIEVAL_METRICS}"
    --responses "${RESPONSES}"
    --run-dir "${RUN_DIR}"
    --backend "${BACKEND}"
    --answer-model "${ANSWER_MODEL}"
    --answer-api-key-env "${API_KEY_ENV}"
    --answer-base-url "${BASE_URL}"
    --context-token-budget "${CONTEXT_TOKEN_BUDGET}"
    --max-retries 8
    --retry-backoff-seconds 5
  )
  if [[ "${RESUME}" == "1" ]]; then
    answer_args+=(--resume)
  fi
  run_step "${answer_args[@]}"
else
  log "SKIP: answer"
  if [[ ! -f "${RESPONSES}" ]]; then
    echo "Missing responses file for --skip-answer: ${RESPONSES}" >&2
    exit 1
  fi
fi

run_step python3 evaluation/personalmem/run.py grade \
  --dataset "${PREPARED}" \
  --store "${WORK_DIR}" \
  --embedding openrouter \
  --model "${EMBEDDING_MODEL}" \
  --dimensions "${EMBEDDING_DIMS}" \
  --output "${SEARCH_RESULTS}" \
  --report "${RETRIEVAL_METRICS}" \
  --responses "${RESPONSES}" \
  --grades "${GRADES}" \
  --csv "${CSV}" \
  --run-dir "${RUN_DIR}" \
  --backend "${BACKEND}" \
  --answer-model "${ANSWER_MODEL}" \
  --context-token-budget "${CONTEXT_TOKEN_BUDGET}" \
  --top-k "${TOP_K}"

print_grade_summary
log "PersonaMem mem0 local v1 pipeline completed"

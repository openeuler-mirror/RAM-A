#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/memory-cases-quick-verify.XXXXXX")"
DEFAULT_DOC_DIR="${ROOT_DIR}/crates/memory-cases/test/accuracy_docs"
RUN_ID="$(date +%Y%m%d%H%M%S)-$$"
API_TOKEN="${MEMORY_CASES_API_TOKEN:-quick-verify-${RUN_ID}-${RANDOM}-${RANDOM}}"
export MEMORY_CASES_API_TOKEN="$API_TOKEN"
AUTH_HEADER=(--header "Authorization: Bearer ${API_TOKEN}")

# 可通过位置参数或环境变量指定导入目录：
#   crates/memory-cases/quick_start_verify.sh /path/to/docs
#   MEMORY_CASES_DOC_DIR=/path/to/docs crates/memory-cases/quick_start_verify.sh
DOC_DIR="${1:-${MEMORY_CASES_DOC_DIR:-$DEFAULT_DOC_DIR}}"

# 默认使用临时 SQLite，保证每次启动都是干净环境；需要保留数据时可设置拆分后的 store。
RAG_STORE="${MEMORY_CASES_RAG_STORE:-${TMP_DIR}/memory-cases.sqlite}"
MEMORY_STORE="${MEMORY_CASES_MEMORY_STORE:-${TMP_DIR}/memory-cases-index.sqlite}"
PORT="${MEMORY_CASES_PORT:-$((20000 + RANDOM % 40000))}"
BASE_URL="http://127.0.0.1:${PORT}"
API_LOG="${TMP_DIR}/api.log"
INGESTOR_LOG="${TMP_DIR}/ingestor.log"
API_PID=""
INGESTOR_PID=""
CLEANED_UP=0

DATASET_ID="${MEMORY_CASES_DATASET_ID:-quick-verify-dataset-${RUN_ID}}"
CHUNK_SIZE="${MEMORY_CASES_CHUNK_SIZE:-160}"
CHAT_TOP_K="${MEMORY_CASES_CHAT_TOP_K:-5}"
SERVER_ARGS=(--rag-store "$RAG_STORE" --memory-store "$MEMORY_STORE" --chunk-size "$CHUNK_SIZE")

DOC_FILES=()
DOCUMENT_IDS=()
TASK_IDS=()

usage() {
  cat <<EOF
Usage:
  crates/memory-cases/quick_start_verify.sh [doc_dir]

Purpose:
  一键启动 memory-cases API 和 ingestor，导入指定目录中的文档，完成基础链路验证后进入交互问答循环。

Environment:
  MEMORY_CASES_DOC_DIR       未传位置参数时使用的文档目录
  MEMORY_CASES_RAG_STORE     业务 SQLite 路径，保存 dataset/document/task/chunk
  MEMORY_CASES_MEMORY_STORE  检索索引 SQLite 路径，保存 memories/FTS/embedding
  MEMORY_CASES_PORT          API 监听端口，默认随机端口
  MEMORY_CASES_DATASET_ID    dataset id，默认带运行号避免重复
  MEMORY_CASES_CHUNK_SIZE    入库切块大小，默认 160
  MEMORY_CASES_CHAT_TOP_K    每次问答取回的引用数，默认 5
  MEMORY_CASES_API_TOKEN     内部 Bearer token，未设置时仅为本次运行生成
  MEMORY_CASES_KEEP_TMP=1    退出后保留临时目录和日志
EOF
}

cleanup() {
  set +e
  [[ "$CLEANED_UP" == "1" ]] && return
  CLEANED_UP=1

  # 清理过程中忽略二次中断，尽量保证后台 API/ingestor 都能被回收。
  trap - EXIT
  trap '' INT TERM

  # 退出时关闭两个后台进程，避免端口、SQLite 连接和轮询进程残留。
  for pid in "$INGESTOR_PID" "$API_PID"; do
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null
    [[ -n "$pid" ]] && wait "$pid" 2>/dev/null
  done

  # 默认清理临时目录；调试启动或入库问题时可设置 MEMORY_CASES_KEEP_TMP=1。
  if [[ "${MEMORY_CASES_KEEP_TMP:-0}" == "1" ]]; then
    echo "kept tmp: $TMP_DIR"
  else
    rm -rf "$TMP_DIR"
  fi
}

handle_interrupt() {
  echo
  echo "Received Ctrl+C; cleaning up background processes..."
  exit 130
}

handle_terminate() {
  echo
  echo "Received termination signal; cleaning up background processes..."
  exit 143
}

trap cleanup EXIT
trap handle_interrupt INT
trap handle_terminate TERM

fail() {
  echo "ERROR: $*" >&2
  echo "---- api log ----" >&2
  tail -n 80 "$API_LOG" >&2 2>/dev/null || true
  echo "---- ingestor log ----" >&2
  tail -n 80 "$INGESTOR_LOG" >&2 2>/dev/null || true
  exit 1
}

get() { curl --noproxy '*' -fsS "${AUTH_HEADER[@]}" "$BASE_URL$1"; }

post_json() {
  curl --noproxy '*' -fsS "${AUTH_HEADER[@]}" "$BASE_URL$1" \
    --json "$2"
}

post_file() {
  local document_id="$1"
  local task_id="$2"
  local doc_file="$3"
  local path="$4"
  local doc_name="${doc_file##*/}"
  local mime_type
  mime_type="$(mime_type_for_file "$doc_file")"

  curl --noproxy '*' -fsS "${AUTH_HEADER[@]}" "$BASE_URL$path" \
    --form-string "id=${document_id}" \
    --form-string "task_id=${task_id}" \
    --form-string "name=${doc_name}" \
    -F "file=@${doc_file};type=${mime_type}"
}

mime_type_for_file() {
  local doc_file="$1"
  local extension="${doc_file##*.}"
  extension="${extension,,}"
  # 当前 parser 支持 Markdown 和纯文本；这里把常见文本后缀归到这两类。
  case "$extension" in
    md|markdown|mdx) printf '%s' "text/markdown" ;;
    txt|text|log) printf '%s' "text/plain" ;;
    *) fail "unsupported document extension: $doc_file" ;;
  esac
}

require_command() {
  command -v "$1" >/dev/null || fail "missing $1"
}

validate_inputs() {
  if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
  fi
  (( $# <= 1 )) || fail "only one doc_dir argument is supported"

  # 先检查本地依赖和参数，避免服务启动到一半才失败。
  require_command cargo
  require_command curl
  require_command find
  require_command sort
  require_command python3

  [[ -d "$DOC_DIR" ]] || fail "doc dir not found: $DOC_DIR"
  [[ -r "$DOC_DIR" ]] || fail "doc dir not readable: $DOC_DIR"
  [[ "$CHUNK_SIZE" =~ ^[0-9]+$ && "$CHUNK_SIZE" -gt 0 ]] ||
    fail "MEMORY_CASES_CHUNK_SIZE must be a positive integer"
  [[ "$CHAT_TOP_K" =~ ^[0-9]+$ && "$CHAT_TOP_K" -gt 0 ]] ||
    fail "MEMORY_CASES_CHAT_TOP_K must be a positive integer"
}

collect_doc_files() {
  # 收集指定目录下可入库的文档；默认只扫一层，避免误把大目录全部导入。
  mapfile -d '' DOC_FILES < <(
    find "$DOC_DIR" -maxdepth 1 -type f \
      \( -iname '*.md' -o -iname '*.markdown' -o -iname '*.mdx' \
      -o -iname '*.txt' -o -iname '*.text' -o -iname '*.log' \) \
      -print0 | sort -z
  )

  (( ${#DOC_FILES[@]} > 0 )) || fail "no supported documents found in $DOC_DIR"
}

print_config() {
  echo "rag_store=$RAG_STORE"
  echo "memory_store=$MEMORY_STORE"
  echo "api=$BASE_URL"
  echo "dataset_id=$DATASET_ID"
  echo "doc_dir=$DOC_DIR"
  echo "doc_count=${#DOC_FILES[@]}"
  echo "chunk_size=$CHUNK_SIZE"
  echo "chat_top_k=$CHAT_TOP_K"
}

start_api() {
  # API 负责接收 dataset、document、search、chat 请求。
  (
    cd "$ROOT_DIR"
    cargo run -p memory-cases -- --api --bind "127.0.0.1:${PORT}" "${SERVER_ARGS[@]}"
  ) >"$API_LOG" 2>&1 &
  API_PID="$!"

  # 后台启动需要一点时间；轮询 /health，确认服务真的开始监听。
  for _ in $(seq 1 120); do
    get /health >/dev/null 2>&1 && return
    kill -0 "$API_PID" 2>/dev/null || fail "api exited"
    sleep 0.25
  done
  get /health >/dev/null || fail "api not healthy"
}

start_ingestor() {
  # ingestor 常驻轮询 pending task，负责解析原文、切 chunk、写业务 SQLite 和 memory-core 索引库。
  (
    cd "$ROOT_DIR"
    cargo run -p memory-cases -- --ingestor "${SERVER_ARGS[@]}" --poll-ms 100
  ) >"$INGESTOR_LOG" 2>&1 &
  INGESTOR_PID="$!"
}

create_dataset() {
  # 创建本次交互使用的 dataset；默认 id 带运行号，便于配合持久化 store 重复启动。
  echo "creating dataset"
  post_json /api/v1/datasets \
    "$(python3 - "$DATASET_ID" <<'PY'
import json
import sys

dataset_id = sys.argv[1]
print(json.dumps({
    "id": dataset_id,
    "name": "Memory Cases Quick Verify Dataset",
    "description": "interactive quick start verification"
}, ensure_ascii=False))
PY
)" >/dev/null || fail "create dataset failed"
}

upload_documents() {
  # 上传文档会创建入库任务；ingestor 可并发消费这些任务，存储层负责等待和重试 SQLite 写锁。
  echo "uploading documents"
  local index=1
  local doc_file
  for doc_file in "${DOC_FILES[@]}"; do
    [[ -r "$doc_file" ]] || fail "file not readable: $doc_file"

    local document_id
    local task_id
    document_id="$(printf 'quick-verify-document-%s-%04d' "$RUN_ID" "$index")"
    task_id="$(printf 'quick-verify-task-%s-%04d' "$RUN_ID" "$index")"
    DOCUMENT_IDS+=("$document_id")
    TASK_IDS+=("$task_id")

    echo "uploading [$index/${#DOC_FILES[@]}] ${doc_file##*/}"
    post_file "$document_id" "$task_id" "$doc_file" "/api/v1/datasets/${DATASET_ID}/documents" \
      >/dev/null || fail "upload failed: $doc_file"
    ((index += 1))
  done
}

wait_for_ingestion() {
  # 等待所有上传任务完成；任一 task failed 时立即打印日志退出。
  echo "waiting for ingestion"
  local pending_tasks=("${TASK_IDS[@]}")
  local last_pending_count=0

  for _ in $(seq 1 600); do
    local next_pending_tasks=()
    local task_id
    for task_id in "${pending_tasks[@]}"; do
      local task
      task="$(get "/api/v1/tasks/${task_id}")"
      [[ "$task" == *'"status":"completed"'* ]] && continue
      [[ "$task" == *'"status":"failed"'* ]] && fail "ingestion failed: $task"
      next_pending_tasks+=("$task_id")
    done

    pending_tasks=("${next_pending_tasks[@]}")
    (( ${#pending_tasks[@]} == 0 )) && return
    if (( ${#pending_tasks[@]} != last_pending_count )); then
      echo "pending ingestion tasks: ${#pending_tasks[@]}"
      last_pending_count="${#pending_tasks[@]}"
    fi
    kill -0 "$INGESTOR_PID" 2>/dev/null || fail "ingestor exited"
    sleep 0.5
  done

  fail "ingestion did not complete; pending: ${pending_tasks[*]}"
}

check_chunks() {
  # 每篇文档至少要生成一个 chunk；这一步确认“上传 -> 入库 -> 可检索”的前置链路正常。
  echo "checking chunks"
  local index
  for index in "${!DOCUMENT_IDS[@]}"; do
    local document_id="${DOCUMENT_IDS[$index]}"
    local doc_file="${DOC_FILES[$index]}"
    local chunks
    chunks="$(get "/api/v1/datasets/${DATASET_ID}/documents/${document_id}/chunks")"
    [[ "$chunks" == *'"total":'* && "$chunks" != *'"total":0'* ]] ||
      fail "expected chunks for ${document_id} (${doc_file##*/})"
  done
}

chat_payload() {
  local question="$1"
  python3 - "$DATASET_ID" "$question" "$CHAT_TOP_K" <<'PY'
import json
import sys

print(json.dumps({
    "dataset_id": sys.argv[1],
    "question": sys.argv[2],
    "top_k": int(sys.argv[3]),
}, ensure_ascii=False))
PY
}

print_chat_result() {
  local raw="$1"
  printf '%s\n' "$raw" | python3 -c '
import json
import sys

raw = sys.stdin.read()
try:
    data = json.loads(raw)
except Exception:
    print(raw)
    raise SystemExit(0)

answer = data.get("answer") or ""
references = data.get("references") or []

print("Answer:")
print(answer if answer else "(no answer)")
print()
print(f"References: {len(references)}")
for index, ref in enumerate(references, 1):
    source = ref.get("source_name") or ref.get("source_path") or ref.get("document_id") or "unknown"
    score = ref.get("score")
    score_text = f" score={score:.4f}" if isinstance(score, (int, float)) else ""
    content = " ".join((ref.get("content") or "").split())
    if len(content) > 220:
        content = content[:220] + "..."
    print(f"[{index}] {source}{score_text}")
    if content:
        print(f"    {content}")
'
}

interactive_loop() {
  echo
  echo "Documents have been imported. You can ask questions now. Type :q, :quit, exit, or quit to exit."

  local question
  local payload
  local chat
  while true; do
    printf '\nQuestion> '
    if ! IFS= read -r question; then
      echo
      break
    fi

    case "${question,,}" in
      :q|:quit|exit|quit) break ;;
    esac
    [[ -n "${question//[[:space:]]/}" ]] || continue

    kill -0 "$API_PID" 2>/dev/null || fail "api exited"
    payload="$(chat_payload "$question")"
    if ! chat="$(post_json /api/v1/chat/completions "$payload" 2>"${TMP_DIR}/last-chat.err")"; then
      echo "Request failed:" >&2
      cat "${TMP_DIR}/last-chat.err" >&2 2>/dev/null || true
      continue
    fi

    print_chat_result "$chat"
  done
}

main() {
  validate_inputs "$@"
  collect_doc_files
  print_config
  start_api
  start_ingestor
  create_dataset
  upload_documents
  wait_for_ingestion
  check_chunks
  interactive_loop
}

main "$@"

#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/memory-cases-qa-eval.XXXXXX")"

# quick_start_verify.sh 用于快速启动和基础链路验证；
# 本脚本在同一文档集上叠加 qa_cases.jsonl 做批量准确性评测。
DEFAULT_DOC_DIR="${ROOT_DIR}/crates/memory-cases/test/accuracy_docs"
DOC_DIR="${MEMORY_CASES_DOC_DIR:-${MEMORY_RAG_DOC_DIR:-$DEFAULT_DOC_DIR}}"
CASES_FILE="${MEMORY_CASES_QA_CASES:-${MEMORY_RAG_QA_CASES:-${ROOT_DIR}/crates/memory-cases/test/qa_cases.jsonl}}"
QA_RUNNER="${ROOT_DIR}/crates/memory-cases/test/qa_eval_runner.py"
REPORT_FILE="${MEMORY_CASES_QA_REPORT:-${MEMORY_RAG_QA_REPORT:-${ROOT_DIR}/outputs/memory-cases/qa_eval_report.json}}"
DOC_LIMIT="${MEMORY_CASES_QA_MAX_DOCS:-${MEMORY_RAG_QA_MAX_DOCS:-0}}"
MIN_SOLUTION_TERM_CASES="${MEMORY_CASES_QA_MIN_SOLUTION_TERM_CASES:-${MEMORY_RAG_QA_MIN_SOLUTION_TERM_CASES:-1}}"

# 每次评测都用临时 SQLite，避免历史入库数据影响召回结果。
RAG_STORE="${MEMORY_CASES_QA_RAG_STORE:-${MEMORY_RAG_QA_RAG_STORE:-${TMP_DIR}/memory-cases.sqlite}}"
MEMORY_STORE="${MEMORY_CASES_QA_MEMORY_STORE:-${MEMORY_RAG_QA_MEMORY_STORE:-${TMP_DIR}/memory-cases-index.sqlite}}"
PORT="${MEMORY_CASES_PORT:-${MEMORY_RAG_PORT:-$((20000 + RANDOM % 40000))}}"
BASE_URL="http://127.0.0.1:${PORT}"
API_LOG="${TMP_DIR}/api.log"
API_PID=""
CONFIG_FILE="${TMP_DIR}/ram-a-mem.json"
DATASET_ID="${MEMORY_CASES_QA_DATASET_ID:-${MEMORY_RAG_QA_DATASET_ID:-qa-eval-dataset}}"

# 默认 chunk size 刻意设小一些，让“现象/根因/解决方案”更可能分散到不同 chunk。
# 这样能测试文档级召回是否真的能把解决方案 chunk 一起带回来。
CHUNK_SIZE="${MEMORY_CASES_QA_CHUNK_SIZE:-${MEMORY_RAG_QA_CHUNK_SIZE:-160}}"
CASE_API_TOKEN="${MEMORY_CASES_API_TOKEN:-memory-cases-qa-eval-token}"
DOC_FILES=()
DOCUMENT_IDS=()
TASK_IDS=()
case_count=0
solution_term_case_count=0

cleanup() {
  set +e
  # API 和 ingestion 位于同一个 ram-a-mem 进程。
  [[ -n "$API_PID" ]] && kill "$API_PID" 2>/dev/null
  [[ -n "$API_PID" ]] && wait "$API_PID" 2>/dev/null

  # 默认清理临时 store 和日志；排查失败时可设置 MEMORY_CASES_QA_KEEP_TMP=1 保留现场。
  if [[ "${MEMORY_CASES_QA_KEEP_TMP:-${MEMORY_RAG_QA_KEEP_TMP:-0}}" == "1" ]]; then
    echo "kept tmp: $TMP_DIR"
  else
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

fail() {
  echo "ERROR: $*" >&2
  # 失败时打印统一服务日志，方便定位启动、入库或检索问题。
  echo "---- api log ----" >&2
  tail -n 80 "$API_LOG" >&2 2>/dev/null || true
  exit 1
}

get() {
  curl --noproxy '*' -fsS "$BASE_URL$1" \
    -H "Authorization: Bearer ${CASE_API_TOKEN}"
}

# REST 调用的小封装，让下面的流程更像“测试步骤清单”。
post_json() {
  curl --noproxy '*' -fsS "$BASE_URL$1" \
    -H "Authorization: Bearer ${CASE_API_TOKEN}" \
    --json "$2"
}

# 上传文档时显式带固定 document_id/task_id，便于后续按 task 轮询入库状态。
post_file() {
  local document_id="$1"
  local task_id="$2"
  local doc_file="$3"
  local path="$4"
  local doc_name="${doc_file##*/}"
  local mime_type
  mime_type="$(mime_type_for_file "$doc_file")"

  curl --noproxy '*' -fsS "$BASE_URL$path" \
    -H "Authorization: Bearer ${CASE_API_TOKEN}" \
    --form-string "id=${document_id}" \
    --form-string "task_id=${task_id}" \
    --form-string "name=${doc_name}" \
    -F "file=@${doc_file};type=${mime_type}"
}

put_file() {
  local task_id="$1"
  local doc_file="$2"
  local path="$3"
  local doc_name="${doc_file##*/}"
  local mime_type
  mime_type="$(mime_type_for_file "$doc_file")"

  curl --noproxy '*' -fsS -X PUT "$BASE_URL$path" \
    -H "Authorization: Bearer ${CASE_API_TOKEN}" \
    --form-string "task_id=${task_id}" \
    --form-string "name=${doc_name}" \
    -F "file=@${doc_file};type=${mime_type}"
}

delete_path() {
  curl --noproxy '*' -fsS -X DELETE "$BASE_URL$1" \
    -H "Authorization: Bearer ${CASE_API_TOKEN}"
}

mime_type_for_file() {
  local doc_file="$1"
  local extension="${doc_file##*.}"
  extension="${extension,,}"
  # 当前 parser 只支持 Markdown 和纯文本，测试文档也只允许这两类格式。
  case "$extension" in
    md|markdown|mdx) printf '%s' "text/markdown" ;;
    txt|text|log) printf '%s' "text/plain" ;;
    *) fail "unsupported test document extension: $doc_file" ;;
  esac
}

contains_doc_file() {
  local expected="$1"
  local doc_file
  for doc_file in "${DOC_FILES[@]}"; do
    [[ "$doc_file" == "$expected" ]] && return 0
  done
  return 1
}

require_command() {
  command -v "$1" >/dev/null || fail "missing $1"
}

validate_inputs() {
  # 先做本地依赖和参数检查，避免服务启动到一半才因为缺命令或路径错误失败。
  require_command cargo
  require_command curl
  require_command find
  require_command sort
  require_command python3

  [[ -d "$DOC_DIR" ]] || fail "doc dir not found: $DOC_DIR"
  [[ -r "$DOC_DIR" ]] || fail "doc dir not readable: $DOC_DIR"
  [[ -f "$CASES_FILE" ]] || fail "qa cases file not found: $CASES_FILE"
  [[ -f "$QA_RUNNER" ]] || fail "qa eval runner not found: $QA_RUNNER"
  [[ "$DOC_LIMIT" =~ ^[0-9]+$ ]] || fail "MEMORY_CASES_QA_MAX_DOCS must be a non-negative integer"
  [[ "$MIN_SOLUTION_TERM_CASES" =~ ^[0-9]+$ ]] || fail "MEMORY_CASES_QA_MIN_SOLUTION_TERM_CASES must be a non-negative integer"
  [[ "$CHUNK_SIZE" =~ ^[0-9]+$ && "$CHUNK_SIZE" -gt 0 ]] || fail "MEMORY_CASES_QA_CHUNK_SIZE must be a positive integer"

  # runner 负责解析 JSON 数组；这里拿到 case 数和解决方案覆盖 case 数作为测试门槛。
  case_count="$(python3 "$QA_RUNNER" count "$CASES_FILE")"
  solution_term_case_count="$(python3 "$QA_RUNNER" solution-count "$CASES_FILE")"
  (( case_count > 0 )) || fail "qa cases file has no cases: $CASES_FILE"
  (( solution_term_case_count >= MIN_SOLUTION_TERM_CASES )) ||
    fail "expected at least ${MIN_SOLUTION_TERM_CASES} qa cases with required_solution_terms, got ${solution_term_case_count}"
}

collect_doc_files() {
  # 先收集测试目录下的所有已支持文本/Markdown 文档；DOC_LIMIT 用于本地快速调试。
  mapfile -d '' DOC_FILES < <(
    find "$DOC_DIR" -maxdepth 1 -type f \( \
      -name '*.txt' -o -name '*.text' -o -name '*.log' -o \
      -name '*.md' -o -name '*.markdown' -o -name '*.mdx' \
    \) -print0 | sort -z
  )
  if (( DOC_LIMIT > 0 && ${#DOC_FILES[@]} > DOC_LIMIT )); then
    DOC_FILES=("${DOC_FILES[@]:0:DOC_LIMIT}")
  fi

  # 即使设置了 DOC_LIMIT，也必须追加 case 声明的 expected_sources，
  # 否则评测可能因为没上传目标文档而产生假失败。
  while IFS= read -r required_doc_name; do
    [[ -n "$required_doc_name" ]] || continue
    required_doc="${DOC_DIR%/}/${required_doc_name}"
    [[ -f "$required_doc" ]] || fail "expected source from cases is missing: $required_doc"
    contains_doc_file "$required_doc" || DOC_FILES+=("$required_doc")
  done < <(python3 "$QA_RUNNER" sources "$CASES_FILE")

  (( ${#DOC_FILES[@]} > 0 )) || fail "no .txt or .md files found in $DOC_DIR"
  assert_doc_formats_present
}

assert_doc_formats_present() {
  # 同时覆盖 Markdown 和纯文本两条解析路径，防止某一种格式悄悄退化。
  local has_text_doc=0
  local has_markdown_doc=0
  local doc_file
  for doc_file in "${DOC_FILES[@]}"; do
    case "${doc_file##*.}" in
      txt|text|log) has_text_doc=1 ;;
      md|markdown|mdx) has_markdown_doc=1 ;;
    esac
  done
  (( has_text_doc == 1 )) || fail "expected at least one text document in $DOC_DIR"
  (( has_markdown_doc == 1 )) || fail "expected at least one markdown document in $DOC_DIR"
}

print_config() {
  # 打印本次评测上下文，失败时可以复现实验输入和报告路径。
  mkdir -p "$(dirname "$REPORT_FILE")"
  echo "rag_store=$RAG_STORE"
  echo "memory_store=$MEMORY_STORE"
  echo "api=$BASE_URL"
  echo "doc_dir=$DOC_DIR"
  echo "case_file=$CASES_FILE"
  echo "report=$REPORT_FILE"
  echo "doc_count=${#DOC_FILES[@]}"
  echo "case_count=$case_count"
  echo "solution_term_case_count=$solution_term_case_count"
}

write_ram_a_config() {
  cat >"$CONFIG_FILE" <<EOF
{
  "auth": {"tokens": [{
    "token_env": "RAM_A_QA_EVAL_MCP_TOKEN",
    "tenant_id": "tenant-local",
    "user_id": "qa-eval",
    "agent_id": "qa-eval",
    "permissions": ["cases:read"]
  }]},
  "features": {"memory": {"enabled": false}, "case_library": {"enabled": true}},
  "http": {
    "bind_address": "127.0.0.1",
    "port": ${PORT},
    "allowed_origins": [],
    "allowed_hosts": ["127.0.0.1:${PORT}"]
  },
  "storage": {"database_path": "${TMP_DIR}/ram-a-memory.sqlite"},
  "providers": {
    "api_key_env": "RAM_A_QA_EVAL_PROVIDER_KEY",
    "base_url": "http://127.0.0.1:1/v1",
    "embedding_provider": "hash",
    "embedding_model": "hash",
    "embedding_dimensions": 32,
    "extractor_model": "unused",
    "verifier_model": "unused"
  },
  "case_library": {
    "rag_store": "${RAG_STORE}",
    "index_store": "${MEMORY_STORE}",
    "api_token_env": "MEMORY_CASES_API_TOKEN",
    "ingestion_poll_ms": 100,
    "embedding_provider": "hash",
    "embedding_model": "hash",
    "embedding_dimensions": 256,
    "chunk_size": ${CHUNK_SIZE},
    "default_library": "ops",
    "libraries": [{
      "name": "ops",
      "dataset_id": "${DATASET_ID}",
      "tenant_ids": ["tenant-local"]
    }]
  }
}
EOF
}

build_ram_a_mem() {
  # 先在前台完成编译，不把首次下载依赖或增量编译耗时计入 API 健康检查超时。
  # cargo run 会复用这里的构建产物，只需负责启动服务。
  echo "building ram-a-mem"
  (
    cd "$ROOT_DIR"
    cargo build -p memory-mcp --bin ram-a-mem
  ) || fail "build ram-a-mem failed"
}

start_api() {
  # ram-a-mem 同时提供案例 API 和后台 ingestion；日志写入临时目录。
  export MEMORY_CASES_API_TOKEN="$CASE_API_TOKEN"
  export RAM_A_QA_EVAL_MCP_TOKEN="ram-a-qa-eval-mcp-token"
  export RAM_A_QA_EVAL_PROVIDER_KEY="unused-provider-key"
  (
    cd "$ROOT_DIR"
    cargo run -p memory-mcp --bin ram-a-mem -- --config "$CONFIG_FILE"
  ) >"$API_LOG" 2>&1 &
  API_PID="$!"

  # 后台启动需要一点时间，这里轮询统一健康端点。
  for _ in $(seq 1 120); do
    get /healthy >/dev/null 2>&1 && return
    kill -0 "$API_PID" 2>/dev/null || fail "api exited"
    sleep 0.25
  done
  get /healthy >/dev/null || fail "api not healthy"
}

create_dataset() {
  # 每次评测都创建固定 dataset，后续上传和查询都使用这个隔离 scope。
  echo "creating dataset"
  post_json /api/v1/datasets \
    "$(printf '{"id":"%s","name":"QA Eval Dataset","description":"memory-cases qa eval test"}' "$DATASET_ID")" \
    >/dev/null || fail "create dataset failed"
}

upload_documents() {
  # 批量上传文档只创建入库任务；真正的解析和写入 memory-core 由进程内 worker 完成。
  # API 和 worker 会并发访问业务 SQLite，存储层负责写锁等待与重试。
  echo "uploading documents"
  local index=1
  local doc_file
  for doc_file in "${DOC_FILES[@]}"; do
    [[ -r "$doc_file" ]] || fail "file not readable: $doc_file"

    local document_id
    local task_id
    document_id="$(printf 'qa-eval-document-%04d' "$index")"
    task_id="$(printf 'qa-eval-task-%04d' "$index")"
    # 记录 task_id 是为了后面逐个轮询，确保所有文档入库完成后再跑 QA。
    DOCUMENT_IDS+=("$document_id")
    TASK_IDS+=("$task_id")

    echo "uploading [$index/${#DOC_FILES[@]}] ${doc_file##*/}"
    post_file "$document_id" "$task_id" "$doc_file" "/api/v1/datasets/${DATASET_ID}/documents" \
      >/dev/null || fail "upload failed: $doc_file"
    ((index += 1))
  done
}

wait_for_task_completed() {
  local task_id="$1"

  for _ in $(seq 1 600); do
    local task
    task="$(get "/api/v1/tasks/${task_id}")"
    [[ "$task" == *'"status":"completed"'* ]] && return
    [[ "$task" == *'"status":"failed"'* ]] && fail "ingestion failed: $task"
    kill -0 "$API_PID" 2>/dev/null || fail "ram-a-mem exited"
    sleep 0.5
  done

  fail "ingestion did not complete: $task_id"
}

wait_for_ingestion() {
  # QA 必须等所有 task completed；只要任一 task failed 就立即打印日志并退出。
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
    kill -0 "$API_PID" 2>/dev/null || fail "ram-a-mem exited"
    sleep 0.5
  done

  fail "ingestion did not complete; pending: ${pending_tasks[*]}"
}

check_chunks() {
  # 确认每篇上传文档都生成了至少一个 chunk，这是后续检索可用的基本前提。
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

run_cases() {
  # 具体 case 执行、解决方案覆盖校验和报告生成都放在 Python runner 里，shell 只传环境。
  echo "running qa cases"
  python3 "$QA_RUNNER" run "$CASES_FILE" "$REPORT_FILE" "$BASE_URL" "$DATASET_ID"
}

search_payload() {
  local query="$1"
  python3 - "$query" <<'PY'
import json
import sys

print(json.dumps({"query": sys.argv[1], "top_k": 5}, ensure_ascii=False))
PY
}

search_dataset_for() {
  local query="$1"
  post_json "/api/v1/datasets/${DATASET_ID}/search" "$(search_payload "$query")"
}

assert_search_has_token() {
  local token="$1"
  local raw
  raw="$(search_dataset_for "$token")"
  [[ "$raw" == *"$token"* ]] || fail "expected search results to contain token ${token}: $raw"
}

assert_search_missing_token() {
  local token="$1"
  local raw
  raw="$(search_dataset_for "$token")"
  [[ "$raw" != *"$token"* ]] || fail "expected search results to omit token ${token}: $raw"
}

check_document_update_delete() {
  # 用临时文档单独验证更新/删除接口，不影响前面的 QA cases 准确率统计。
  echo "checking document update/delete"
  local document_id="qa-eval-update-delete-document"
  local create_task_id="qa-eval-update-delete-create-task"
  local update_task_id="qa-eval-update-delete-update-task"
  local old_file="${TMP_DIR}/update-delete-old.txt"
  local new_file="${TMP_DIR}/update-delete-new.txt"
  local chunks
  local documents

  printf '%s\n' "updatetestoldneedle old searchable content before replacement." >"$old_file"
  printf '%s\n' "updatetestnewneedle new searchable content after replacement." >"$new_file"

  post_file "$document_id" "$create_task_id" "$old_file" "/api/v1/datasets/${DATASET_ID}/documents" \
    >/dev/null || fail "upload update/delete fixture failed"
  wait_for_task_completed "$create_task_id"
  assert_search_has_token "updatetestoldneedle"

  # PUT 创建新的 ingestion task；内嵌 worker 会持续消费并替换旧索引。
  put_file "$update_task_id" "$new_file" "/api/v1/datasets/${DATASET_ID}/documents/${document_id}" \
    >/dev/null || fail "update document fixture failed"
  wait_for_task_completed "$update_task_id"
  assert_search_missing_token "updatetestoldneedle"
  assert_search_has_token "updatetestnewneedle"

  delete_path "/api/v1/datasets/${DATASET_ID}/documents/${document_id}" \
    >/dev/null || fail "delete document fixture failed"
  chunks="$(get "/api/v1/datasets/${DATASET_ID}/documents/${document_id}/chunks")"
  [[ "$chunks" == *'"total":0'* ]] || fail "expected chunks to be empty after delete: $chunks"
  documents="$(get "/api/v1/datasets/${DATASET_ID}/documents")"
  [[ "$documents" != *"$document_id"* ]] || fail "expected document to be removed after delete: $documents"
  assert_search_missing_token "updatetestnewneedle"
}

main() {
  # 主流程保持线性：准备输入 -> 启动服务 -> 入库文档 -> 执行 QA 评测。
  validate_inputs
  collect_doc_files
  print_config
  write_ram_a_config
  build_ram_a_mem
  start_api
  create_dataset
  upload_documents
  wait_for_ingestion
  check_chunks
  run_cases
  check_document_update_delete
  echo "memory-cases qa eval test passed"
}

main "$@"

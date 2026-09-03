# RAM-A RPM + xiaoO 容器端到端自测

本文用于在干净的 openEuler 容器中安装 RAM-A RPM，先通过真实 HTTP MCP 验证
`ram-a-mem`，再接入 xiaoO 验证自动记忆摄入和召回。命令默认 RAM-A、xiaoO 在同一容器中
运行；容器内不依赖 systemd，服务直接作为后台进程启动。

本文不假定未确认的 RPM 发布 URL。执行前由测试人员提供 RAM-A RPM URL 或本地 RPM；模型
服务必须提供 OpenAI-compatible `/chat/completions`。Embedding 使用本地 hash，把外部依赖
限制为 Chat 模型。基础验收不强制测试 TLS、Graph、案例库和 Rerank。

## 测试对象与版本门禁

RPM 验收和源码自动化测试验证的是不同对象：

- `cargo test` 会编译当前源码并在测试进程中使用 mock 或进程内 HTTP Router，不能证明系统中
  已安装的 RPM 包含相同代码。
- 本文通过 `/usr/bin/ram-a-mem`（或 RPM 安装出的实际路径）发起真实 HTTP 请求，验证的是 RPM
  二进制、交付配置、运行时依赖和持久化行为，但不能替代内部边界和故障注入单元测试。
- 发布仓中已有的旧 RPM 只能用于旧版本回归。Rerank `fail_open`、HTTP 并发限制和 Pipeline
  阶段错误契约必须使用包含对应源码提交的候选 RPM 才能作为交付验收结论。

测试前必须记录 RPM 的 NEVRA 和 SHA-256，并从打包流水线记录确认其源码提交。RAM-A 当前二进制
没有提供可用于核对提交号的 `--version` 输出，仅凭文件时间或包名不能证明包含某次修改。如果
候选 RPM 尚未构建，应先运行下列源码测试；这些结果标记为“源码验证”，不能标记为“RPM 验收”：

```bash
cd /path/to/RAM-A

# Rerank fail-closed/fail-open、重试分类。
cargo test -p memory-core hybrid_search_fail_
cargo test -p memory-core retry_classification_is_limited_to_transient_failures

# HTTP 限流、并发上限和 MCP Session 生命周期。
cargo test -p memory-mcp --test http_mcp \
  concurrent_tool_limit_rejects_excess_work_without_queueing -- --exact
cargo test -p memory-mcp --test http_mcp \
  tool_rate_limit_is_scoped_to_the_authenticated_principal_and_tool -- --exact
cargo test -p memory-mcp --test http_mcp \
  active_session_cap_is_enforced_per_principal -- --exact
cargo test -p memory-mcp --test http_mcp \
  configured_idle_timeout_is_applied_to_the_rmcp_session_worker -- --exact

# Pipeline fail_fast、阶段日志以及对外错误结构。
cargo test -p memory-pipeline --test offline_pipeline \
  fail_fast_controls_extraction_and_grounding_failures -- --exact
cargo test -p memory-pipeline --test pipeline_logging
cargo test -p memory-mcp --lib \
  service::tests::provider_failures_keep_pipeline_stage_and_rerank_classification -- --exact
cargo test -p memory-mcp --lib \
  mcp_server::tests::structured_service_errors_expose_stable_pipeline_and_rerank_contracts -- --exact
```

若要形成 RPM 验收结论，先由打包流水线从同一源码提交产生候选 RPM，再执行本文后续命令。当前
RAM-A 源码仓中没有 RPM spec 文件，因此不能在本仓库内用一条通用的 `rpmbuild` 命令可靠地产出
正式 RPM；RPM 的源码提交关系应由实际发行版打包仓或构建流水线提供。

## 1. 启动容器

以下命令在宿主机执行。Podman 可替换为 Docker；本地模型在宿主机时保留 `--network host`：

```bash
export CONTAINER_IMAGE="${CONTAINER_IMAGE:-openeuler/openeuler:24.03-lts}"
podman run --rm -it --name ram-a-e2e --network host "$CONTAINER_IMAGE" bash
```

后续命令均在容器内执行：

```bash
set -euo pipefail
dnf install -y \
  ca-certificates curl git jq openssl sqlite procps-ng \
  findutils sed gawk coreutils gcc gcc-c++ make \
  pkgconf-pkg-config openssl-devel
mkdir -p /root/ram-a-selftest/results /var/lib/ram-a /var/log/ram-a
```

构建 xiaoO 需要 Rust；如果镜像没有 Rust：

```bash
if ! command -v cargo >/dev/null 2>&1; then
  dnf install -y rust cargo
fi
cargo --version
rustc --version
```

若发行版仓库的 Rust 版本不满足 xiaoO，应改用项目认可的 toolchain，不要在验收记录中隐藏
编译器版本变化。

## 2. 下载并安装 RAM-A RPM

从发布地址下载：

```bash
: "${RAM_A_RPM_URL:?请设置 RAM_A_RPM_URL}"
curl --fail --location --retry 3 "$RAM_A_RPM_URL" -o /tmp/ram-a.rpm
```

若使用本地 RPM，在启动容器时把它挂载为 `/tmp/ram-a.rpm`，跳过下载。随后执行：

```bash
sha256sum /tmp/ram-a.rpm | tee /root/ram-a-selftest/results/ram-a-rpm.sha256
rpm -qip /tmp/ram-a.rpm | tee /root/ram-a-selftest/results/ram-a-rpm-info.txt
dnf install -y /tmp/ram-a.rpm

RAM_A_PACKAGE=$(rpm -qp --queryformat '%{NAME}' /tmp/ram-a.rpm)
rpm -ql "$RAM_A_PACKAGE" | tee /root/ram-a-selftest/results/ram-a-rpm-files.txt
rpm -V "$RAM_A_PACKAGE"

RAM_A_BIN=$(command -v ram-a-mem)
test -n "$RAM_A_BIN"
"$RAM_A_BIN" --help | tee /root/ram-a-selftest/results/ram-a-help.txt
rpm -ql "$RAM_A_PACKAGE" | grep -E '/systemd/|\.service$' || true
```

通过判据：RPM 安装无依赖错误，`rpm -V` 没有非预期输出，`ram-a-mem` 可执行。

## 3. 验证模型服务

在同一个容器 shell 中设置：

```bash
: "${MODEL_BASE_URL:?例如 http://127.0.0.1:8000/v1}"
: "${CHAT_MODEL:?请设置实际模型名}"
: "${LLM_API_KEY:?远程服务填真实 key；免认证本地端点也要填非空占位值}"
export MODEL_BASE_URL CHAT_MODEL LLM_API_KEY
export RAM_A_XIAOO_TOKEN="$(openssl rand -hex 32)"
```

先把模型故障与 RAM-A 故障分离：

```bash
curl --fail --silent --show-error \
  -H "Authorization: Bearer $LLM_API_KEY" \
  -H 'Content-Type: application/json' \
  "$MODEL_BASE_URL/chat/completions" \
  -d "$(jq -nc --arg model "$CHAT_MODEL" \
    '{model:$model,messages:[{role:"user",content:"只回复 OK"}],temperature:0,max_tokens:16}')" \
  | tee /root/ram-a-selftest/results/model-smoke.json \
  | jq -e '.choices[0].message.content | length > 0'
```

## 4. 创建 RAM-A 配置

只开启个人记忆，密钥值不写入配置：

```bash
install -d -m 0750 /etc/ram-a /var/lib/ram-a
jq -n --arg base_url "$MODEL_BASE_URL" --arg model "$CHAT_MODEL" '{
  auth:{tokens:[{
    token_env:"RAM_A_XIAOO_TOKEN",tenant_id:"tenant-e2e",
    user_id:"user-e2e",agent_id:"xiaoo",
    permissions:["memory:read","memory:write"]
  }]},
  features:{
    memory:{enabled:true},case_library:{enabled:false},graph_memory:{enabled:false}
  },
  http:{
    bind_address:"127.0.0.1",port:18081,allowed_origins:[],
    allowed_hosts:["127.0.0.1:18081"],tls_termination_acknowledged:false
  },
  limits:{
    max_body_bytes:16777216,requests_per_second:20,rate_burst:40,
    max_in_flight_per_principal_tool:4,initialize_requests_per_second:4,
    initialize_rate_burst:8,max_active_sessions_per_principal:8,
    max_active_sessions_global:256,session_idle_timeout_seconds:1800
  },
  pipeline:{
    fail_fast:true,max_memory_chars:500,
    max_candidate_tokens:320,max_window_tokens:640,
    extractor_max_output_tokens:1600,verifier_max_output_tokens:1000,
    extractor_context_window_tokens:null,verifier_context_window_tokens:null,
    reasoning_reserve_tokens:0
  },
  storage:{database_path:"/var/lib/ram-a/ram-a-memory.sqlite"},
  providers:{
    api_key_env:"LLM_API_KEY",base_url:$base_url,
    embedding_provider:"hash",embedding_api_key_env:null,embedding_base_url:null,
    embedding_model:"hash",embedding_dimensions:1024,
    extractor_model:$model,verifier_model:$model,timeout_seconds:120,max_retries:3,
    reasoning_effort:null,enable_thinking:null,
    send_temperature:true,temperature:0,
    output_token_parameter:"max_tokens",structured_output:"prompt_only",
    reasoning_only_retry:false,json_repair_attempts:0
  },
  retrieval:{
    mode:"hybrid",embedding_weight:0.7,bm25_weight:0.3,candidate_k:100,
    rerank:{
      enabled:false,provider:"openrouter",model:"cohere/rerank-v3.5",
      api_key_env:null,base_url:"http://127.0.0.1:19090/v1",
      input_k:40,timeout_ms:30000,fail_open:false
    }
  },
  case_library:null,graph_memory:null
}' >/etc/ram-a/ram-a-mem.json

chmod 0640 /etc/ram-a/ram-a-mem.json
jq empty /etc/ram-a/ram-a-mem.json
```

如果使用已验证的 GLM Coding Plan，可把 `providers.reasoning_effort` 设为 `"none"`，
`reasoning_only_retry` 设为 `true`，`json_repair_attempts` 设为 `1`。如果目标服务支持
`enable_thinking=false` 而不支持 `reasoning_effort`，只能配置 `enable_thinking`，两者不能同时设置。
`max_tokens` 的单位是输出 token，不是字数或汉字个数。

## 5. 启动 RAM-A

```bash
export RUST_LOG=info
"$RAM_A_BIN" --config /etc/ram-a/ram-a-mem.json \
  >/var/log/ram-a/ram-a-mem.jsonl 2>&1 &
RAM_A_PID=$!
echo "$RAM_A_PID" >/run/ram-a-mem.pid

for attempt in $(seq 1 30); do
  curl --fail --silent http://127.0.0.1:18081/ready >/dev/null && break
  if ! kill -0 "$RAM_A_PID" 2>/dev/null; then
    cat /var/log/ram-a/ram-a-mem.jsonl
    exit 1
  fi
  sleep 1
done

curl --fail --silent http://127.0.0.1:18081/healthy \
  | tee /root/ram-a-selftest/results/healthy.txt
curl --fail --silent http://127.0.0.1:18081/ready \
  | tee /root/ram-a-selftest/results/ready.txt
```

通过判据：进程存活，`/healthy` 和 `/ready` 均返回 2xx。

## 6. 创建 MCP 调用脚本

脚本完成 initialize、initialized notification 和 `tools/call`，兼容 JSON 与 SSE 响应：

```bash
cat >/root/ram-a-selftest/mcp.sh <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
: "${RAM_A_XIAOO_TOKEN:?RAM_A_XIAOO_TOKEN is required}"
BASE_URL="${RAM_A_MCP_URL:-http://127.0.0.1:18081/mcp}"
STATE_DIR="${RAM_A_MCP_STATE_DIR:-/root/ram-a-selftest}"
SESSION_FILE="$STATE_DIR/mcp-session-id"

normalize_response() {
  local raw="$1"
  if jq -e . "$raw" >/dev/null 2>&1; then
    cat "$raw"
  else
    sed -n 's/^data: //p' "$raw" | tail -n 1 | jq .
  fi
}

common_headers=(
  -H 'Content-Type: application/json'
  -H 'Accept: application/json, text/event-stream'
  -H "Authorization: Bearer $RAM_A_XIAOO_TOKEN"
  -H 'X-Agent-ID: xiaoo'
  -H 'MCP-Protocol-Version: 2025-11-25'
)

case "${1:-}" in
  init)
    curl --fail --silent --show-error -D "$STATE_DIR/init.headers" \
      -o "$STATE_DIR/init.raw" "${common_headers[@]}" -X POST "$BASE_URL" \
      -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"ram-a-selftest","version":"1.0"}}}'
    awk -F': *' 'tolower($1)=="mcp-session-id" {gsub("\r","",$2); print $2}' \
      "$STATE_DIR/init.headers" | tail -n 1 >"$SESSION_FILE"
    test -s "$SESSION_FILE"
    normalize_response "$STATE_DIR/init.raw"
    curl --fail --silent --show-error -o /dev/null "${common_headers[@]}" \
      -H "Mcp-Session-Id: $(cat "$SESSION_FILE")" -X POST "$BASE_URL" \
      -d '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}'
    ;;
  list)
    curl --fail --silent --show-error -o "$STATE_DIR/call.raw" \
      "${common_headers[@]}" -H "Mcp-Session-Id: $(cat "$SESSION_FILE")" \
      -X POST "$BASE_URL" \
      -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'
    normalize_response "$STATE_DIR/call.raw"
    ;;
  call)
    tool="${2:?tool name required}"
    request_id="${3:?request id required}"
    arguments="${4:?arguments JSON required}"
    jq -e . <<<"$arguments" >/dev/null
    payload=$(jq -nc --argjson id "$request_id" --arg name "$tool" \
      --argjson arguments "$arguments" \
      '{jsonrpc:"2.0",id:$id,method:"tools/call",params:{name:$name,arguments:$arguments}}')
    curl --fail --silent --show-error -o "$STATE_DIR/call.raw" \
      "${common_headers[@]}" -H "Mcp-Session-Id: $(cat "$SESSION_FILE")" \
      -X POST "$BASE_URL" -d "$payload"
    normalize_response "$STATE_DIR/call.raw"
    ;;
  *)
    echo "usage: $0 init | list | call TOOL REQUEST_ID ARGUMENTS_JSON" >&2
    exit 2
    ;;
esac
EOF
chmod 0700 /root/ram-a-selftest/mcp.sh
```

初始化并确认工具：

```bash
/root/ram-a-selftest/mcp.sh init \
  | tee /root/ram-a-selftest/results/mcp-initialize.json \
  | jq -e '.result.protocolVersion == "2025-11-25"'
/root/ram-a-selftest/mcp.sh list \
  | tee /root/ram-a-selftest/results/mcp-tools.json \
  | jq -e '[.result.tools[].name] | index("memory_ingest") != null and index("memory_search") != null'
```

## 7. 验证无 candidate 路径

```bash
NO_CANDIDATE_ARGS='{
  "conversation_id":"e2e-context-only",
  "messages":[{
    "id":"context-1","role":"user",
    "text":"这条消息只作为上下文，不应产生记忆。","candidate":false
  }]
}'
/root/ram-a-selftest/mcp.sh call memory_ingest 10 "$NO_CANDIDATE_ARGS" \
  | tee /root/ram-a-selftest/results/ingest-no-candidate.json \
  | jq -e '
      .result.isError != true and
      .result.structuredContent.accepted_count == 0 and
      .result.structuredContent.rejected_count == 0 and
      .result.structuredContent.quarantined_count == 0 and
      (.result.structuredContent.memory_ids | length) == 0 and
      .result.structuredContent.idempotency_hit == false'
```

## 8. 验证七阶段摄入

```bash
MARKER="RAMA-E2E-$(date +%s)"
export MARKER
INGEST_ARGS=$(jq -nc --arg marker "$MARKER" '{
  conversation_id:"e2e-conversation-1",
  messages:[
    {id:"context-1",role:"assistant",text:"你平时喜欢什么饮品？",candidate:false},
    {id:"candidate-1",role:"user",speaker:"Alice",
     text:("我的长期测试代号是 " + $marker + "，我明确喜欢喝绿茶。"),
     timestamp:"2026-08-17T10:00:00Z",candidate:true}
  ]
}')
/root/ram-a-selftest/mcp.sh call memory_ingest 11 "$INGEST_ARGS" \
  | tee /root/ram-a-selftest/results/ingest-first.json
jq -e '
  .result.isError != true and
  .result.structuredContent.accepted_count >= 1 and
  (.result.structuredContent.memory_ids | length) >= 1 and
  .result.structuredContent.idempotency_hit == false
' /root/ram-a-selftest/results/ingest-first.json
```

若请求成功但 accepted 为 0，先检查 rejected/quarantined 计数和阶段日志。这表示模型结果未通过
Evidence、枚举或 Grounding 校验，不应直接判定 HTTP、认证或 SQLite 失败。

## 9. 验证幂等缓存和冲突

```bash
/root/ram-a-selftest/mcp.sh call memory_ingest 12 "$INGEST_ARGS" \
  | tee /root/ram-a-selftest/results/ingest-cached.json
jq -e '.result.isError != true and .result.structuredContent.idempotency_hit == true' \
  /root/ram-a-selftest/results/ingest-cached.json
diff \
  <(jq -S '.result.structuredContent.memory_ids' /root/ram-a-selftest/results/ingest-first.json) \
  <(jq -S '.result.structuredContent.memory_ids' /root/ram-a-selftest/results/ingest-cached.json)

CONFLICT_ARGS=$(jq -nc --arg marker "$MARKER" '{
  conversation_id:"e2e-conversation-1",
  messages:[{id:"candidate-1",role:"user",
    text:("修改后的冲突内容 " + $marker),candidate:true}]
}')
/root/ram-a-selftest/mcp.sh call memory_ingest 13 "$CONFLICT_ARGS" \
  | tee /root/ram-a-selftest/results/ingest-conflict.json
jq -e '
  .result.isError == true and
  .result.structuredContent.code == "IDEMPOTENCY_CONFLICT"
' /root/ram-a-selftest/results/ingest-conflict.json
```

## 10. 验证记忆检索

```bash
SEARCH_ARGS=$(jq -nc --arg marker "$MARKER" '{query:($marker + " 绿茶"),top_k:10}')
/root/ram-a-selftest/mcp.sh call memory_search 20 "$SEARCH_ARGS" \
  | tee /root/ram-a-selftest/results/search.json
jq -e --arg marker "$MARKER" '
  .result.isError != true and
  (.result.structuredContent.memories | length) >= 1 and
  any(.result.structuredContent.memories[]; .text | contains($marker))
' /root/ram-a-selftest/results/search.json

FILTER_ARGS=$(jq -nc --arg marker "$MARKER" \
  '{query:($marker + " 绿茶"),top_k:10,memory_types:["preference"]}')
/root/ram-a-selftest/mcp.sh call memory_search 21 "$FILTER_ARGS" \
  | tee /root/ram-a-selftest/results/search-preference.json \
  | jq '.result.structuredContent.memories'
```

类型过滤结果用于观察；模型可能把 marker 和偏好拆成不同记忆，不要求所有模型都在此返回一条
`preference`。

## 11. 验证 SQLite 和重启持久化

```bash
sqlite3 /var/lib/ram-a/ram-a-memory.sqlite '.tables' \
  | tee /root/ram-a-selftest/results/sqlite-tables.txt
sqlite3 /var/lib/ram-a/ram-a-memory.sqlite 'SELECT COUNT(*) FROM memories;' \
  | tee /root/ram-a-selftest/results/sqlite-memory-count.txt
sqlite3 /var/lib/ram-a/ram-a-memory.sqlite \
  'SELECT status, COUNT(*) FROM mcp_ingest_idempotency GROUP BY status ORDER BY status;' \
  | tee /root/ram-a-selftest/results/sqlite-idempotency-count.txt

kill "$RAM_A_PID"
wait "$RAM_A_PID" || true
"$RAM_A_BIN" --config /etc/ram-a/ram-a-mem.json \
  >>/var/log/ram-a/ram-a-mem.jsonl 2>&1 &
RAM_A_PID=$!
echo "$RAM_A_PID" >/run/ram-a-mem.pid
for attempt in $(seq 1 30); do
  curl --fail --silent http://127.0.0.1:18081/ready >/dev/null && break
  sleep 1
done

/root/ram-a-selftest/mcp.sh init >/root/ram-a-selftest/results/mcp-reinitialize.json
/root/ram-a-selftest/mcp.sh call memory_search 30 "$SEARCH_ARGS" \
  | tee /root/ram-a-selftest/results/search-after-restart.json
jq -e --arg marker "$MARKER" '
  any(.result.structuredContent.memories[]; .text | contains($marker))
' /root/ram-a-selftest/results/search-after-restart.json
```

## 12. 验证阶段日志

```bash
jq -r '
  select(.fields.event == "ram_a.memory.ingest.stage.started" or
         .fields.event == "ram_a.memory.ingest.stage.completed" or
         .fields.event == "ram_a.memory.ingest.stage.failed")
  | [.fields.event,.fields.stage,(.fields.error_code // "-"),(.fields.elapsed_ms // "-")]
  | @tsv
' /var/log/ram-a/ram-a-mem.jsonl \
  | tee /root/ram-a-selftest/results/ingest-stage-logs.tsv

for stage in normalize episode window extract validate ground aggregate; do
  grep -q $'completed\t'"$stage"$'\t' \
    /root/ram-a-selftest/results/ingest-stage-logs.tsv
done
if grep -q "$MARKER" /var/log/ram-a/ram-a-mem.jsonl; then
  echo "FAIL: marker leaked into service log" >&2
  exit 1
fi
```

通过判据：成功摄入存在七个规范阶段的 completed，日志不包含测试消息 marker。

## 13. 可选：验证 Extract 故障和 pending 重试

把 Provider 临时指向不可连接端口，验证 `fail_fast=true` 的稳定错误：

```bash
cp /etc/ram-a/ram-a-mem.json /etc/ram-a/ram-a-mem.good.json
jq '
  .providers.base_url="http://127.0.0.1:9/v1" |
  .providers.timeout_seconds=2 |
  .providers.max_retries=1
' /etc/ram-a/ram-a-mem.good.json >/etc/ram-a/ram-a-mem.json

kill "$RAM_A_PID"
wait "$RAM_A_PID" || true
"$RAM_A_BIN" --config /etc/ram-a/ram-a-mem.json \
  >>/var/log/ram-a/ram-a-mem.jsonl 2>&1 &
RAM_A_PID=$!
for attempt in $(seq 1 30); do
  curl --fail --silent http://127.0.0.1:18081/ready >/dev/null && break
  sleep 1
done

FAIL_ARGS=$(jq -nc --arg marker "$MARKER" '{
  conversation_id:"e2e-provider-failure",
  messages:[{id:"provider-failure-1",role:"user",
    text:("请记住故障恢复标记 " + $marker),candidate:true}]
}')
/root/ram-a-selftest/mcp.sh init >/dev/null
/root/ram-a-selftest/mcp.sh call memory_ingest 40 "$FAIL_ARGS" \
  | tee /root/ram-a-selftest/results/ingest-extract-failure.json
jq -e '
  .result.isError == true and
  .result.structuredContent.code == "PIPELINE_FAILED" and
  .result.structuredContent.stage == "extract" and
  .result.structuredContent.retriable == true
' /root/ram-a-selftest/results/ingest-extract-failure.json
```

恢复配置并使用完全相同的请求重试：

```bash
mv /etc/ram-a/ram-a-mem.good.json /etc/ram-a/ram-a-mem.json
kill "$RAM_A_PID"
wait "$RAM_A_PID" || true
"$RAM_A_BIN" --config /etc/ram-a/ram-a-mem.json \
  >>/var/log/ram-a/ram-a-mem.jsonl 2>&1 &
RAM_A_PID=$!
for attempt in $(seq 1 30); do
  curl --fail --silent http://127.0.0.1:18081/ready >/dev/null && break
  sleep 1
done
/root/ram-a-selftest/mcp.sh init >/dev/null
/root/ram-a-selftest/mcp.sh call memory_ingest 41 "$FAIL_ARGS" \
  | tee /root/ram-a-selftest/results/ingest-after-provider-recovery.json
jq -e '.result.isError != true' \
  /root/ram-a-selftest/results/ingest-after-provider-recovery.json
```

该用例证明 Pipeline 失败后的 pending 幂等记录允许相同内容重试。模型是否接受该记忆仍由
Extract、Validate 和 Ground 的结果决定。

## 14. 安装或构建 xiaoO

有 xiaoO RPM 时：

```bash
if [[ -n "${XIAOO_RPM_URL:-}" ]]; then
  curl --fail --location --retry 3 "$XIAOO_RPM_URL" -o /tmp/xiaoo.rpm
  sha256sum /tmp/xiaoo.rpm | tee /root/ram-a-selftest/results/xiaoo-rpm.sha256
  rpm -qip /tmp/xiaoo.rpm | tee /root/ram-a-selftest/results/xiaoo-rpm-info.txt
  dnf install -y /tmp/xiaoo.rpm
fi
```

没有 Agent RPM 时，从目标仓库构建：

```bash
if ! command -v xiaoo >/dev/null 2>&1; then
  git clone --depth 1 https://gitcode.com/openeuler/xiaoO.git /opt/xiaoO
  git -C /opt/xiaoO rev-parse HEAD \
    | tee /root/ram-a-selftest/results/xiaoo-commit.txt
  cargo build --manifest-path /opt/xiaoO/Cargo.toml \
    -p xiaoo-endside --bin xiaoo --release
  install -m 0755 /opt/xiaoO/target/release/xiaoo /usr/local/bin/xiaoo
fi

XIAOO_BIN=$(command -v xiaoo)
"$XIAOO_BIN" --help | tee /root/ram-a-selftest/results/xiaoo-help.txt
```

如果目标版本的 package、binary 或 CLI 参数变化，以该版本 `Cargo.toml` 和 `xiaoo --help`
为准；不要把猜测的构建参数记录为已验证接口。

## 15. 配置 xiaoO

```bash
install -d -m 0700 /etc/xiaoo /var/lib/xiaoo
cat >/etc/xiaoo/mcp.json <<'EOF'
{
  "mcpServers": {
    "ram-a": {
      "transport": "streamable_http",
      "url": "http://127.0.0.1:18081/mcp",
      "bearer_token_env": "RAM_A_XIAOO_TOKEN",
      "agent_id": "xiaoo",
      "timeout_ms": 30000
    }
  }
}
EOF

cat >/etc/xiaoo/config.toml <<EOF
[llm]
provider = "anthropic"
api_base = "$MODEL_BASE_URL"
model = "$CHAT_MODEL"
api_key_env = "LLM_API_KEY"
max_tokens = 8192
reasoning_effort = "none"

[memory_automation]
enabled = true
server = "ram-a"
recall_top_k = 5
recall_token_budget = 512
context_messages = 4
queue_path = "/var/lib/xiaoo/memory-automation-queue.jsonl"
queue_capacity = 256
max_retries = 5
retry_backoff_ms = 250
allowed_agent_roles = ["main", "defaultagent"]
EOF
chmod 0600 /etc/xiaoo/mcp.json /etc/xiaoo/config.toml
```

`memory_automation` 是 xiaoO 配置，不是 MCP 协议或 RAM-A 配置。未启用时，xiaoO 仍可以看到
`memory_ingest/memory_search` 工具，但不会自动在每轮前后执行召回和摄入。

## 16. 验证 xiaoO 自动摄入和召回

使用一个没有被前面裸 MCP 摄入过的新 marker，避免把已有记忆误判为 xiaoO 自动摄入成功。
第一轮要求回复复述 marker，确保最终 assistant 消息也带有可观察信息：

```bash
AGENT_MARKER="XIAOO-E2E-$(date +%s)"
export AGENT_MARKER
"$XIAOO_BIN" --cli \
  --mcp-config /etc/xiaoo/mcp.json \
  run --config /etc/xiaoo/config.toml --debug \
  -p "请记住：我的长期测试代号是 $AGENT_MARKER，我喜欢喝绿茶。请在回复中准确复述这两项。" \
  2>&1 | tee /root/ram-a-selftest/results/xiaoo-ingest-turn.log
```

等待自动摄入队列处理，并通过裸 MCP 排除“Agent 回答了但没有摄入”：

```bash
AGENT_SEARCH_ARGS=$(jq -nc --arg marker "$AGENT_MARKER" \
  '{query:($marker + " 绿茶"),top_k:10}')
for attempt in $(seq 1 30); do
  /root/ram-a-selftest/mcp.sh init >/dev/null
  /root/ram-a-selftest/mcp.sh call memory_search $((100 + attempt)) "$AGENT_SEARCH_ARGS" \
    >/root/ram-a-selftest/results/search-after-xiaoo.json
  if jq -e --arg marker "$AGENT_MARKER" '
      any(.result.structuredContent.memories[]?; .text | contains($marker))
    ' /root/ram-a-selftest/results/search-after-xiaoo.json >/dev/null; then
    break
  fi
  sleep 1
done
jq -e --arg marker "$AGENT_MARKER" '
  any(.result.structuredContent.memories[]?; .text | contains($marker))
' /root/ram-a-selftest/results/search-after-xiaoo.json
```

第二轮不重复 marker，验证自动 recall：

```bash
"$XIAOO_BIN" --cli \
  --mcp-config /etc/xiaoo/mcp.json \
  run --config /etc/xiaoo/config.toml --debug \
  -p '我之前告诉你的长期测试代号和饮品偏好分别是什么？请依据长期记忆回答。' \
  2>&1 | tee /root/ram-a-selftest/results/xiaoo-recall-turn.log

grep -q "$AGENT_MARKER" /root/ram-a-selftest/results/xiaoo-recall-turn.log
grep -q '绿茶' /root/ram-a-selftest/results/xiaoo-recall-turn.log
```

通过判据：

1. xiaoO debug 日志显示 `ram-a` MCP server 连接成功；
2. 第一轮结束后，裸 MCP 能检索到包含 marker 的记忆；
3. 第二轮问题不含 marker，但最终回答正确给出 marker 和绿茶；
4. RAM-A 日志能观察到相应 search/ingest 请求且不包含消息正文；
5. `/var/lib/xiaoo/memory-automation-queue.jsonl` 不持续累积未处理任务。

若第一轮裸 MCP 已检索成功而第二轮回答失败，问题位于 xiaoO recall 注入、角色过滤、Token
预算或 Agent Prompt，而不是 RAM-A 摄入和存储。若裸 MCP 也检索不到，应检查
`memory_automation`、当前 agent role 和摄入队列重试日志。

## 17. 结果归档和清理

不要归档 Token、模型 API Key 或生产对话。当前测试使用专用 marker，可以保留测试响应和脱敏
日志：

```bash
tar -C /root -czf /tmp/ram-a-selftest-results.tar.gz ram-a-selftest/results
sha256sum /tmp/ram-a-selftest-results.tar.gz
kill "$RAM_A_PID" 2>/dev/null || true
wait "$RAM_A_PID" 2>/dev/null || true
unset RAM_A_XIAOO_TOKEN LLM_API_KEY
```

验收记录至少包含：容器镜像、RAM-A RPM NEVRA/SHA-256、xiaoO commit 或 RPM NEVRA、模型名、
配置 SHA-256、每步通过/失败和结果归档 SHA-256。模型质量问题应与协议、认证、存储和 Agent
调度问题分别记录。

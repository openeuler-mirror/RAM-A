#!/usr/bin/env bash
set -euo pipefail

: "${GLM_CODING_TOKEN:?GLM_CODING_TOKEN is required}"
: "${OPENROUTER_API_KEY:?OPENROUTER_API_KEY is required}"
: "${RAM_A_XIAOO_TOKEN:?RAM_A_XIAOO_TOKEN is required}"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
state_dir="${RAM_A_MCP_STATE_DIR:-/var/lib/ram-a/selftest}"
results_dir="$state_dir/results"
log_file=/var/log/ram-a/ram-a-mem.jsonl
mkdir -p "$results_dir"

jq -e '
  .pipeline.extractor_max_output_tokens == 1600 and
  .pipeline.verifier_max_output_tokens == 1000 and
  .pipeline.extractor_context_window_tokens == null and
  .pipeline.verifier_context_window_tokens == null and
  .pipeline.reasoning_reserve_tokens == 0 and
  .providers.reasoning_effort == "none" and
  .providers.enable_thinking == null and
  .providers.output_token_parameter == "max_tokens" and
  .providers.structured_output == "prompt_only" and
  .providers.reasoning_only_retry == true and
  .providers.json_repair_attempts == 1
' /etc/ram-a/ram-a-mem.json >/dev/null

model_payload="$(jq -nc '{model:"GLM-5.2",messages:[{role:"user",content:"只回复 OK"}],temperature:0,max_tokens:64,reasoning_effort:"none"}')"
curl --fail --silent --show-error --retry 5 --retry-all-errors --retry-delay 2 \
  -H "Authorization: Bearer $GLM_CODING_TOKEN" \
  -H 'Content-Type: application/json' \
  https://open.bigmodel.cn/api/coding/paas/v4/chat/completions \
  -d "$model_payload" >"$results_dir/model-smoke.json"
jq -e '
  .choices[0].message
  | ((.content // "") | length) > 0
    and ((has("reasoning_content") | not) or (.reasoning_content | type == "string"))
' "$results_dir/model-smoke.json" >/dev/null

"$script_dir/mcp.sh" init >"$results_dir/mcp-initialize.json"
jq -e '.result.protocolVersion == "2025-11-25"' "$results_dir/mcp-initialize.json" >/dev/null
"$script_dir/mcp.sh" list >"$results_dir/mcp-tools.json"
jq -e '[.result.tools[].name] | index("memory_ingest") != null and index("memory_search") != null' \
  "$results_dir/mcp-tools.json" >/dev/null

marker="RAMA-ARM64-E2E-$(date +%s)-$$"
conversation_id="arm64-e2e-$marker"
ingest_args="$(jq -nc --arg marker "$marker" --arg conversation "$conversation_id" '{
  conversation_id:$conversation,
  messages:[
    {id:"context-1",role:"assistant",text:"你平时喜欢什么饮品？",candidate:false},
    {id:"candidate-1",role:"user",speaker:"Alice",text:("我的长期测试代号是 " + $marker + "，我明确喜欢喝绿茶。"),timestamp:"2026-09-02T10:00:00Z",candidate:true}
  ]
}')"

"$script_dir/mcp.sh" call memory_ingest 11 "$ingest_args" >"$results_dir/ingest-first.json"
jq -e '.result.isError != true and .result.structuredContent.accepted_count >= 1 and (.result.structuredContent.memory_ids | length) >= 1 and .result.structuredContent.idempotency_hit == false' \
  "$results_dir/ingest-first.json" >/dev/null

"$script_dir/mcp.sh" call memory_ingest 12 "$ingest_args" >"$results_dir/ingest-cached.json"
jq -e '.result.isError != true and .result.structuredContent.idempotency_hit == true' \
  "$results_dir/ingest-cached.json" >/dev/null
diff \
  <(jq -S '.result.structuredContent.memory_ids | sort' "$results_dir/ingest-first.json") \
  <(jq -S '.result.structuredContent.memory_ids | sort' "$results_dir/ingest-cached.json")

search_args="$(jq -nc --arg marker "$marker" '{query:($marker + " 绿茶"),top_k:10}')"
"$script_dir/mcp.sh" call memory_search 13 "$search_args" >"$results_dir/search.json"
jq -e --arg marker "$marker" 'any(.result.structuredContent.memories[]?; .text | contains($marker))' \
  "$results_dir/search.json" >/dev/null

for stage in normalize episode window extract validate ground aggregate; do
  jq -e --arg stage "$stage" \
    'select(.fields.event == "ram_a.memory.ingest.stage.completed" and .fields.stage == $stage)' \
    "$log_file" >/dev/null
done

for secret in "$GLM_CODING_TOKEN" "$OPENROUTER_API_KEY"; do
  if grep -R -F -- "$secret" "$results_dir" "$log_file" >/dev/null 2>&1; then
    echo "credential material was found in verification output" >&2
    exit 1
  fi
done

echo "RAM_A_INGEST_OK marker=$marker"

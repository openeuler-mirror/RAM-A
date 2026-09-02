#!/usr/bin/env bash
set -euo pipefail

: "${RAM_A_XIAOO_TOKEN:?RAM_A_XIAOO_TOKEN is required}"

base_url="${RAM_A_MCP_URL:-http://127.0.0.1:18081/mcp}"
state_dir="${RAM_A_MCP_STATE_DIR:-/var/lib/ram-a/selftest}"
session_file="$state_dir/mcp-session-id"
mkdir -p "$state_dir"

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
    curl --fail --silent --show-error -D "$state_dir/init.headers" \
      -o "$state_dir/init.raw" "${common_headers[@]}" -X POST "$base_url" \
      -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"ram-a-image-selftest","version":"1.0"}}}'
    awk -F': *' 'tolower($1)=="mcp-session-id" {gsub("\r","",$2); print $2}' \
      "$state_dir/init.headers" | tail -n 1 >"$session_file"
    test -s "$session_file"
    normalize_response "$state_dir/init.raw"
    curl --fail --silent --show-error -o /dev/null "${common_headers[@]}" \
      -H "Mcp-Session-Id: $(cat "$session_file")" -X POST "$base_url" \
      -d '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}'
    ;;
  list)
    curl --fail --silent --show-error -o "$state_dir/call.raw" \
      "${common_headers[@]}" -H "Mcp-Session-Id: $(cat "$session_file")" \
      -X POST "$base_url" \
      -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'
    normalize_response "$state_dir/call.raw"
    ;;
  call)
    tool="${2:?tool name required}"
    request_id="${3:?request id required}"
    arguments="${4:?arguments JSON required}"
    jq -e . <<<"$arguments" >/dev/null
    payload="$(jq -nc --argjson id "$request_id" --arg name "$tool" \
      --argjson arguments "$arguments" \
      '{jsonrpc:"2.0",id:$id,method:"tools/call",params:{name:$name,arguments:$arguments}}')"
    curl --fail --silent --show-error -o "$state_dir/call.raw" \
      "${common_headers[@]}" -H "Mcp-Session-Id: $(cat "$session_file")" \
      -X POST "$base_url" -d "$payload"
    normalize_response "$state_dir/call.raw"
    ;;
  *)
    echo "usage: $0 init | list | call TOOL REQUEST_ID ARGUMENTS_JSON" >&2
    exit 2
    ;;
esac

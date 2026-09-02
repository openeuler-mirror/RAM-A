#!/usr/bin/env bash
set -euo pipefail

: "${ANTHROPIC_AUTH_TOKEN:?ANTHROPIC_AUTH_TOKEN is required}"
: "${RAM_A_XIAOO_TOKEN:?RAM_A_XIAOO_TOKEN is required}"

exec xiaoo --cli --mcp-config /etc/xiaoo/mcp.json \
  run --config /etc/xiaoo/config.toml --debug "$@"

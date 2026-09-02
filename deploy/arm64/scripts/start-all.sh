#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
"$script_dir/start-ram-a.sh"

if [[ "${1:-}" == "verify" ]]; then
  shift
  exec "$script_dir/verify-ingest.sh" "$@"
fi

exec "$script_dir/start-xiaoo.sh" "$@"

#!/usr/bin/env bash
set -euo pipefail

: "${GLM_CODING_TOKEN:?GLM_CODING_TOKEN is required}"
: "${RAM_A_XIAOO_TOKEN:?RAM_A_XIAOO_TOKEN is required}"

install -d -m 0750 /var/lib/ram-a /var/lib/ram-a/selftest/results /var/log/ram-a /run/ram-a

pid_file=/run/ram-a/ram-a-mem.pid
log_file=/var/log/ram-a/ram-a-mem.jsonl

if [[ -s "$pid_file" ]] && kill -0 "$(cat "$pid_file")" 2>/dev/null; then
  exit 0
fi

/usr/local/bin/ram-a-mem --config /etc/ram-a/ram-a-mem.json >>"$log_file" 2>&1 &
ram_a_pid=$!
echo "$ram_a_pid" >"$pid_file"

for _ in $(seq 1 120); do
  if curl --fail --silent http://127.0.0.1:18081/ready >/dev/null 2>&1; then
    exit 0
  fi
  if ! kill -0 "$ram_a_pid" 2>/dev/null; then
    cat "$log_file" >&2
    exit 1
  fi
  sleep 1
done

echo "RAM-A did not become ready within 120 seconds" >&2
tail -n 200 "$log_file" >&2
exit 1

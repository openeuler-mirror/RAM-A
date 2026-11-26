#!/usr/bin/env bash
# 端到端验证 ram-a-kv 通过 SDK 调用 actrail-kv receiver 采集 turn_end 事件。
# 流程：构建 .so + daemon -> 启动 daemon（SDK 模式）-> 发 turn_end 带 actrail_capture
#       -> 等 SDK submit 落盘 -> 查询 captures.sqlite 断言记录存在。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d -t ram-a-kv-sdk-test-XXXXXX)"
trap 'rm -rf "$WORK"; kill "${DAEMON_PID:-}" 2>/dev/null || true' EXIT

SO="$ROOT/actrail-kv/target/release/libactrail_receiver.so"
DAEMON_BIN="$ROOT/target/release/ram-a-kv"

echo "== 测试目录: $WORK =="
echo "== .so: $SO =="
echo "== daemon: $DAEMON_BIN =="

# 1. 检查产物
if [[ ! -f "$SO" ]]; then
    echo "FAIL: $SO 不存在，先运行: cargo build --release -p actrail-kv-sdk-receiver (在 actrail-kv/ 下)"
    exit 1
fi
if [[ ! -f "$DAEMON_BIN" ]]; then
    echo "FAIL: $DAEMON_BIN 不存在，先运行: cargo build --release -p ram-a-kv (在 ram-a-kv/ 下)"
    exit 1
fi

# 2. 准备 actrail-kv settings.toml（指向临时目录的 captures.sqlite）
#    SDK 默认读 ~/.actrail/actrail-kv/settings.toml，这里通过 HOME 重定向到临时目录
mkdir -p "$WORK/home/.actrail/actrail-kv"
cat > "$WORK/home/.actrail/actrail-kv/settings.toml" <<'EOF'
[storage]
backend = "sqlite"

[storage.retention]
max_age_seconds = 604800
max_size_bytes = 268435456
EOF

# 3. 准备 daemon 配置
cat > "$WORK/daemon.toml" <<EOF
listen_addr = "127.0.0.1:6998"
noop_backend = true
actrail_sdk_lib = "$SO"
actrail_model_deployment_key = "test-deployment"
actrail_kv_namespace = "test-ns"
EOF

# 4. 启动 daemon（HOME 重定向，使 SDK 读到临时目录的 settings）
echo "== 启动 daemon =="
RAM_A_KV_CONFIG="$WORK/daemon.toml" HOME="$WORK/home" "$DAEMON_BIN" >"$WORK/daemon.log" 2>&1 &
DAEMON_PID=$!

# 等待 daemon 就绪
for i in $(seq 1 30); do
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
        echo "FAIL: daemon 提前退出，日志："
        cat "$WORK/daemon.log"
        exit 1
    fi
    if curl -sf -X POST http://127.0.0.1:6998/event -H 'Content-Type: application/json' \
           -d '{"type":"health"}' | grep -q '"ok":true'; then
        break
    fi
    sleep 0.2
done

echo "== daemon 健康检查通过 =="
grep "actrail-kv sdk enabled" "$WORK/daemon.log" && echo "✓ SDK 加载成功" || {
    echo "FAIL: 未找到 SDK 启用日志，daemon 日志："
    cat "$WORK/daemon.log"
    exit 1
}

# 5. 发 turn_end 事件，带 actrail_capture
echo "== 发送 turn_end 事件（带 actrail_capture）=="
RESPONSE=$(curl -sf -X POST http://127.0.0.1:6998/event -H 'Content-Type: application/json' -d '{
    "type": "turn_end",
    "session_id": "test-session-1",
    "kv_transfer_params": {"chunk_hashes": ["hash-a", "hash-b"]},
    "actrail_capture": {
        "endpoint_key": "test-endpoint",
        "agent_key": "test-agent",
        "payload": {
            "model": "test-model",
            "messages": [{"role": "user", "content": "hello"}]
        }
    }
}')
echo "$RESPONSE" | python3 -m json.tool 2>/dev/null || echo "$RESPONSE"

echo "$RESPONSE" | grep -q '"ok":true' || {
    echo "FAIL: turn_end 返回失败"
    exit 1
}
echo "✓ turn_end 成功"

# 6. 等待 SDK submit 落盘（fire-and-forget，需轮询）
echo "== 等待 SDK submit 落盘 =="
SQLITE_PATH="$WORK/home/.actrail/actrail-kv/captures.sqlite"
DEADLINE=$(( $(date +%s) + 10 ))
RECORD_COUNT=0
while [[ $(date +%s) -lt $DEADLINE ]]; do
    if [[ -f "$SQLITE_PATH" ]]; then
        RECORD_COUNT=$(sqlite3 "$SQLITE_PATH" "SELECT COUNT(*) FROM captures;" 2>/dev/null || echo 0)
        if [[ "$RECORD_COUNT" -ge 1 ]]; then
            break
        fi
    fi
    sleep 0.3
done

if [[ "$RECORD_COUNT" -lt 1 ]]; then
    echo "FAIL: 等待 10s 后 captures.sqlite 中无记录"
    echo "== daemon 日志 =="
    cat "$WORK/daemon.log"
    echo "== sqlite 路径: $SQLITE_PATH =="
    ls -la "$WORK/home/.actrail/actrail-kv/" 2>/dev/null || echo "(目录不存在)"
    exit 1
fi
echo "✓ captures.sqlite 中有 $RECORD_COUNT 条记录"

# 7. 验证记录内容（schema: id, session_key, captured_at, record）
echo "== 验证记录内容 =="
sqlite3 -json "$SQLITE_PATH" "SELECT id, session_key, captured_at, record FROM captures LIMIT 1;" | python3 -c "
import json, sys
rows = json.load(sys.stdin)
assert len(rows) == 1, f'expected 1 row, got {len(rows)}'
r = rows[0]
assert r['session_key'] == 'test-session-1', f'session_key={r[\"session_key\"]}'
rec = json.loads(r['record'])
assert rec['source'] == 'ram-a-kv-relay', f'source={rec[\"source\"]}'
assert rec['session_key'] == 'test-session-1', f'rec.session_key={rec[\"session_key\"]}'
comp = rec['comparison']
assert comp['endpoint_key'] == 'test-endpoint', f'endpoint_key={comp[\"endpoint_key\"]}'
assert comp['agent_key'] == 'test-agent', f'agent_key={comp[\"agent_key\"]}'
assert comp['model_deployment_key'] == 'test-deployment', f'model_deployment_key={comp[\"model_deployment_key\"]}'
assert comp['kv_namespace'] == 'test-ns', f'kv_namespace={comp[\"kv_namespace\"]}'
payload = rec['payload']
assert payload['model'] == 'test-model', f'payload.model={payload[\"model\"]}'
assert payload['messages'][0]['content'] == 'hello', 'payload.messages mismatch'
print('✓ session_key = test-session-1')
print('✓ source = ram-a-kv-relay')
print('✓ comparison.endpoint_key = test-endpoint')
print('✓ comparison.agent_key = test-agent')
print('✓ comparison.model_deployment_key = test-deployment (daemon 补齐)')
print('✓ comparison.kv_namespace = test-ns (daemon 补齐)')
print('✓ payload.model = test-model')
print('✓ payload.messages[0].content = hello')
"

# 8. 再发一个不含 actrail_capture 的 turn_end，确认不影响采集
echo "== 发送 turn_end 事件（无 actrail_capture，应不影响采集）=="
curl -sf -X POST http://127.0.0.1:6998/event -H 'Content-Type: application/json' -d '{
    "type": "turn_end",
    "session_id": "test-session-1",
    "kv_transfer_params": {"chunk_hashes": ["hash-c"]}
}' | grep -q '"ok":true' && echo "✓ turn_end 成功"

sleep 1
RECORD_COUNT2=$(sqlite3 "$SQLITE_PATH" "SELECT COUNT(*) FROM captures;" 2>/dev/null || echo 0)
if [[ "$RECORD_COUNT2" -ne 1 ]]; then
    echo "FAIL: 无 actrail_capture 的 turn_end 后记录数应为 1，实际 $RECORD_COUNT2"
    exit 1
fi
echo "✓ 无 actrail_capture 的 turn_end 不产生采集记录（仍为 1 条）"

echo ""
echo "============================================"
echo "  ✅ 全部测试通过"
echo "============================================"

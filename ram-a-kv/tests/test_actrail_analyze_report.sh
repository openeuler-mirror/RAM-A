#!/usr/bin/env bash
# 端到端验证 ram-a-kv 通过 SDK 暴露的 analyze 和 report 接口。
# 流程：启动 daemon（receiver + analyzer + reporter 三个 .so）
#       → 发 turn_end 采集若干请求 → POST /actrail/analyze → POST /actrail/report
#       → 验证 analysis.json 和 report.html 生成正确。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d -t ram-a-kv-analyze-test-XXXXXX)"
trap 'rm -rf "$WORK"; kill "${DAEMON_PID:-}" 2>/dev/null || true' EXIT

RECEIVER_SO="$ROOT/actrail-kv/target/release/libactrail_receiver.so"
ANALYZER_SO="$ROOT/actrail-kv/target/release/libactrail_analyzer.so"
REPORTER_SO="$ROOT/actrail-kv/target/release/libactrail_reporter.so"
DAEMON_BIN="$ROOT/target/release/ram-a-kv"

echo "== 测试目录: $WORK =="

# 1. 检查产物
for f in "$RECEIVER_SO" "$ANALYZER_SO" "$REPORTER_SO" "$DAEMON_BIN"; do
    [[ -f "$f" ]] || { echo "FAIL: $f 不存在"; exit 1; }
done

# 2. 准备 actrail-kv settings.toml（HOME 重定向到临时目录）
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
actrail_sdk_lib = "$RECEIVER_SO"
actrail_analyzer_lib = "$ANALYZER_SO"
actrail_reporter_lib = "$REPORTER_SO"
actrail_model_deployment_key = "test-deployment"
actrail_kv_namespace = "test-ns"
EOF

# 4. 启动 daemon
echo "== 启动 daemon =="
RAM_A_KV_CONFIG="$WORK/daemon.toml" HOME="$WORK/home" "$DAEMON_BIN" >"$WORK/daemon.log" 2>&1 &
DAEMON_PID=$!
for i in $(seq 1 30); do
    kill -0 "$DAEMON_PID" 2>/dev/null || { echo "FAIL: daemon exited"; cat "$WORK/daemon.log"; exit 1; }
    curl -sf -X POST http://127.0.0.1:6998/event -H 'Content-Type: application/json' \
         -d '{"type":"health"}' | grep -q '"ok":true' && break
    sleep 0.2
done
grep -q "actrail-kv sdk enabled" "$WORK/daemon.log" && echo "✓ SDK 加载成功" || { echo "FAIL: SDK 未加载"; cat "$WORK/daemon.log"; exit 1; }

# 5. 采集 4 条相似请求（不同 workspace 名，相同长指令 — 足以触发模板发现）
echo "== 采集 4 条相似请求 =="
for ws in billing search orders accounts; do
    curl -sf -X POST http://127.0.0.1:6998/event -H 'Content-Type: application/json' -d "{
        \"type\": \"turn_end\",
        \"session_id\": \"session-$ws\",
        \"kv_transfer_params\": {\"chunk_hashes\": [\"hash-$ws\"]},
        \"actrail_capture\": {
            \"endpoint_key\": \"test-endpoint\",
            \"agent_key\": \"test-agent\",
            \"payload\": {
                \"model\": \"test-model\",
                \"messages\": [
                    {\"role\": \"system\", \"content\": \"Workspace: $ws. Follow the repository rules, read the code before making changes, and run the relevant tests before completion.\"},
                    {\"role\": \"user\", \"content\": \"Fix the failing validation test.\"}
                ]
            }
        }
    }" | grep -q '"ok":true' || { echo "FAIL: turn_end for $ws failed"; exit 1; }
done
echo "✓ 4 条请求采集成功"

# 等待 SDK submit 落盘
sleep 2
SQLITE_PATH="$WORK/home/.actrail/actrail-kv/captures.sqlite"
COUNT=$(sqlite3 "$SQLITE_PATH" "SELECT COUNT(*) FROM captures;" 2>/dev/null || echo 0)
[[ "$COUNT" -ge 4 ]] || { echo "FAIL: expected >=4 records, got $COUNT"; exit 1; }
echo "✓ captures.sqlite 中有 $COUNT 条记录"

# 6. POST /actrail/analyze
echo "== 调用 /actrail/analyze =="
ANALYZE_RESP=$(curl -sf -X POST http://127.0.0.1:6998/actrail/analyze \
    -H 'Content-Type: application/json' \
    -d '{"top_k": 20, "output": "analysis.json"}')
echo "$ANALYZE_RESP" | python3 -c "
import json, sys
r = json.load(sys.stdin)
assert r['ok'], f'analyze failed: {r.get(\"error\")}'
out = r['data']['output']
assert 'analysis.json' in out, f'unexpected output path: {out}'
print(f'✓ analysis 输出: {out}')
" || exit 1

# 验证 analysis.json 文件存在且是有效 JSON
ANALYSIS_PATH=$(echo "$ANALYZE_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin)['data']['output'])")
[[ -f "$ANALYSIS_PATH" ]] || { echo "FAIL: $ANALYSIS_PATH 不存在"; exit 1; }
python3 -c "
import json
with open('$ANALYSIS_PATH') as f:
    a = json.load(f)
assert 'run' in a, 'missing run'
assert 'templates' in a, 'missing templates'
assert 'defects' in a, 'missing defects'
print(f'✓ analysis.json 有效: {len(a[\"templates\"])} templates, {len(a[\"defects\"])} defects')
" || exit 1

# 7. POST /actrail/report (html)
echo "== 调用 /actrail/report (html) =="
REPORT_RESP=$(curl -sf -X POST http://127.0.0.1:6998/actrail/report \
    -H 'Content-Type: application/json' \
    -d "{\"analysis\": \"$ANALYSIS_PATH\", \"format\": \"html\", \"output\": \"report.html\"}")
echo "$REPORT_RESP" | python3 -c "
import json, sys
r = json.load(sys.stdin)
assert r['ok'], f'report failed: {r.get(\"error\")}'
outputs = r['data']['outputs']
assert len(outputs) == 1, f'expected 1 output, got {len(outputs)}'
assert outputs[0]['format'] == 'html', f'wrong format: {outputs[0][\"format\"]}'
print(f'✓ report 输出: {outputs[0][\"output\"]}')
" || exit 1

REPORT_PATH=$(echo "$REPORT_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin)['data']['outputs'][0]['output'])")
[[ -f "$REPORT_PATH" ]] || { echo "FAIL: $REPORT_PATH 不存在"; exit 1; }
grep -q "TTFT\|命中率\|KV" "$REPORT_PATH" || { echo "FAIL: report.html 内容不包含预期关键词"; exit 1; }
echo "✓ report.html 内容有效"

# 8. POST /actrail/report (both html + md)
echo "== 调用 /actrail/report (both) =="
REPORT2_RESP=$(curl -sf -X POST http://127.0.0.1:6998/actrail/report \
    -H 'Content-Type: application/json' \
    -d "{\"analysis\": \"$ANALYSIS_PATH\", \"output\": \"report2\"}")
echo "$REPORT2_RESP" | python3 -c "
import json, sys
r = json.load(sys.stdin)
assert r['ok'], f'report failed: {r.get(\"error\")}'
outputs = r['data']['outputs']
formats = sorted(o['format'] for o in outputs)
assert formats == ['html', 'md'], f'expected [html, md], got {formats}'
print(f'✓ 双格式报告: {[o[\"output\"] for o in outputs]}')
" || exit 1

[[ -f "$WORK/report2.html" ]] || { echo "FAIL: report2.html 不存在, path=$WORK/report2.html"; ls -la "$WORK/"; exit 1; }
[[ -f "$WORK/report2.md" ]] || { echo "FAIL: report2.md 不存在, path=$WORK/report2.md"; ls -la "$WORK/"; exit 1; }
echo "✓ report2.html + report2.md 均生成"

# 9. 测试 analyze 筛选参数
echo "== 测试 analyze 筛选参数 (session_keys) =="
ANALYZE2_RESP=$(curl -sf -X POST http://127.0.0.1:6998/actrail/analyze \
    -H 'Content-Type: application/json' \
    -d '{"session_keys": ["session-billing"], "output": "analysis2.json"}')
echo "$ANALYZE2_RESP" | grep -q '"ok":true' || { echo "FAIL: analyze with session_keys failed"; exit 1; }
ANALYSIS2_PATH=$(echo "$ANALYZE2_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin)['data']['output'])")
[[ -f "$ANALYSIS2_PATH" ]] || { echo "FAIL: $ANALYSIS2_PATH 不存在"; exit 1; }
echo "✓ 筛选分析成功"

# 10. 测试未配置时的错误响应
echo "== 测试 analyzer 未配置时的错误 =="
# 需要一个不配 analyzer_lib 的 daemon 实例（用不同端口）
cat > "$WORK/daemon2.toml" <<EOF
listen_addr = "127.0.0.1:6999"
noop_backend = true
actrail_sdk_lib = "$RECEIVER_SO"
actrail_model_deployment_key = "test"
actrail_kv_namespace = "test"
EOF
RAM_A_KV_CONFIG="$WORK/daemon2.toml" HOME="$WORK/home" "$DAEMON_BIN" >"$WORK/daemon2.log" 2>&1 &
DAEMON2_PID=$!
for i in $(seq 1 20); do
    curl -sf -X POST http://127.0.0.1:6999/event -d '{"type":"health"}' 2>/dev/null | grep -q '"ok":true' && break
    sleep 0.2
done
ERR_RESP=$(curl -sf -X POST http://127.0.0.1:6999/actrail/analyze -H 'Content-Type: application/json' -d '{}')
echo "$ERR_RESP" | grep -q 'actrail_analyzer_lib not configured' || { echo "FAIL: expected not configured error, got: $ERR_RESP"; exit 1; }
echo "✓ 未配置时返回错误"
kill "$DAEMON2_PID" 2>/dev/null || true

echo ""
echo "============================================"
echo "  ✅ 全部测试通过"
echo "============================================"

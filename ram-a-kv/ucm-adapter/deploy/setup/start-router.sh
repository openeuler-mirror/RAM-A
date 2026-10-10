#!/usr/bin/env bash
# 启动 vllm-router(DP4 会话亲和):同一个会话的请求按 x-session-id 稳定哈希到同一张卡,
# 预取请求(/kv/prefetch)走**同一个** consistent_hash 策略 ⇒ 预取落点 == 推理落点。
#
# 依赖
#   - router 源码含 UCM 预取透传端点
#     (见 ../../patches/03-router-kv-prefetch-passthrough.patch)
#   - 已 cargo build --release(见 ./router-cargo-config.toml 的国内镜像加速)
#   - 后端 vllm 引擎已就绪
#
# 用法
#   BACKEND_URL=http://127.0.0.1:8000 ./start-router.sh    # 指定后端(默认同样指向 :8000)
set -uo pipefail

ROUTER_DIR=${ROUTER_DIR:-$HOME/ucm/router}
PORT=${ROUTER_PORT:-9003}
BACKEND_URL=${BACKEND_URL:-http://127.0.0.1:8000}
PROM_PORT=${ROUTER_PROM_PORT:-29003}
DP=${DP_SIZE:-4}
LOG=${ROUTER_LOG:-/tmp/vllm-router.log}

# 预取端点扇出:worker 侧 UCM 预取 HTTP 服务监听 base_port + dp*tp_size + tp。
# 必须与 ucm-dsv4.yaml 的 prefetch_http.base_port、以及 vllm 的 --tensor-parallel-size 一致。
export UCM_PREFETCH_BASE_PORT=${UCM_PREFETCH_BASE_PORT:-6997}
export UCM_PREFETCH_TP_SIZE=${UCM_PREFETCH_TP_SIZE:-4}

cd "$ROUTER_DIR" || { echo "router 目录不存在: $ROUTER_DIR"; exit 1; }

nohup ./target/release/vllm-router \
  --host 127.0.0.1 --port "$PORT" \
  --worker-urls "$BACKEND_URL" \
  --policy consistent_hash \
  --intra-node-data-parallel-size "$DP" \
  --prometheus-port "$PROM_PORT" \
  > "$LOG" 2>&1 &

echo "router pid=$!  listen=127.0.0.1:$PORT  backend=$BACKEND_URL  log=$LOG"
echo "就绪检查: curl -s http://127.0.0.1:$PORT/health"
echo "预取端点: POST http://127.0.0.1:$PORT/kv/prefetch  (body: {\"block_ids\":[...]})"
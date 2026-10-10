#!/usr/bin/env bash
# ==========================================================================
# TP8/DP1 引擎启动器:4 张 910C(8 die) + TP8/DP1  ←→  另一部署为 8 卡 16 die + TP4/DP4
#
# 与 ../setup/start_vllm_serve.sh 的差异只有三处(其余参数完全一致,保证收益可比):
#   ① ASCEND_RT_VISIBLE_DEVICES=0..7   只暴露 4 张卡 = 8 个 die(等价目标机器规格)
#      注意:本机 1 个 die = 1 个 torch device(npu-smi 的 8 NPU x 2 Chip = 16 die = 16 device),
#      所以「4 张 910C」要写 8 个 die 号;只写 0,1,2,3 会暴露 4 个 die,TP8 起不来:
#        AssertionError: DP adjusted local rank 4 is out of bounds for 4 devices
#   ② --tensor-parallel-size  8
#   ③ --data-parallel-size    1
#   ④ UCM_CONFIG_FILE 指向本目录渲染出来的 TP8 配置(容量由环境变量决定)
#
# 用法(必须在容器内执行;HOST 侧用 docker exec):
#   UCM_CACHE_BUFFER_CAPACITY_GB=64 ./start-tp8.sh up     # 起引擎(后台)
#   ./start-tp8.sh ready                                  # 等 :8000 就绪
#   ./start-tp8.sh down                                   # 停引擎(含孤儿进程)
#
# 环境变量:
#   UCM_CACHE_BUFFER_CAPACITY_GB  节点级 DDR 缓存(GB),默认 64。会渲染进 UCM 配置。
#   ASCEND_VISIBLE_CARDS          参与的 die 号,默认 0,1,2,3,4,5,6,7(8 die = 4 张卡)。
#   VLLM_GPU_MEM_UTIL             显存利用率,默认 0.9。TP8/DP1 下 EP 只有 8 路(TP4/DP4 为 16),
#                                 每 die 分摊的专家权重翻倍 ⇒ 1M 上下文所需 KV(12.28GiB)超过
#                                 0.9 留给 KV 的 10.13GiB,需抬到 0.95。
#   VLLM_MAX_MODEL_LEN            最大上下文,默认 1048576。抬利用率仍不够时可降到 524288。
# ==========================================================================
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
CAP=${UCM_CACHE_BUFFER_CAPACITY_GB:-64}
CARDS=${ASCEND_VISIBLE_CARDS:-0,1,2,3,4,5,6,7}
MEMUTIL=${VLLM_GPU_MEM_UTIL:-0.9}
MAXLEN=${VLLM_MAX_MODEL_LEN:-1048576}
OUT=${UCM_TP8_OUT:-/mnt/sdb/ucm/real-test}
LOGDIR=$OUT/logs
CFG=$OUT/ucm-dsv4-tp8-${CAP}g.yaml
LOG=$LOGDIR/vllm-tp8-${CAP}g-ml${MAXLEN}-u${MEMUTIL}.log
MODEL=${VLLM_MODEL_PATH:-/mnt/sdb/models/DeepSeek-V4-Flash-0731-w8a8}

mkdir -p "$LOGDIR"

cmd_up() {
  sed "s/__CAP_GB__/$CAP/" "$HERE/ucm-dsv4-tp8.yaml.tmpl" > "$CFG"
  export HCCL_BUFFSIZE=1024
  export VLLM_ENGINE_READY_TIMEOUT_S=7200
  export ASCEND_RT_VISIBLE_DEVICES=$CARDS
  cd /                       # 避免 cwd 下的 vllm/ 目录被解析为命名空间包
  echo "[start-tp8] 卡=$CARDS  TP=8  DP=1  cache_buffer_capacity_gb=$CAP  memutil=$MEMUTIL  max_model_len=$MAXLEN"
  echo "[start-tp8] 配置=$CFG"
  echo "[start-tp8] 日志=$LOG"
  nohup vllm serve "$MODEL" \
    --host 0.0.0.0 --served-model-name deepseek \
    --tokenizer-mode deepseek_v4 --reasoning-parser deepseek_v4 \
    --tool-call-parser deepseek_v4 --enable-auto-tool-choice \
    --max-model-len "$MAXLEN" --quantization ascend \
    --tensor-parallel-size 8 --data-parallel-size 1 --enable-expert-parallel \
    --block-size 32 --gpu-memory-utilization "$MEMUTIL" \
    --max-num-batched-tokens 10240 --max-num-seqs 64 \
    --speculative-config '{"method": "dspark", "num_speculative_tokens": 3, "enforce_eager": true}' \
    --model-loader-extra-config '{"enable_multithread_load": true, "num_threads": 128}' \
    --additional-config '{"ascend_compilation_config": {"enable_npugraph_ex": true, "enable_static_kernel": false}, "enable_cpu_binding": false, "enable_dsa_cp": true, "enable_flashcomm1": true, "multistream_overlap_shared_expert": true}' \
    --kv-transfer-config "{\"kv_connector\": \"UCMConnector\", \"kv_connector_module_path\": \"ucm.integration.vllm.ucm_connector\", \"kv_role\": \"kv_both\", \"kv_connector_extra_config\": {\"UCM_CONFIG_FILE\": \"$CFG\"}}" \
    > "$LOG" 2>&1 &
  echo "[start-tp8] pid=$!"
}

cmd_ready() {
  local t=0
  while [ $t -lt 3600 ]; do
    if curl -sf -m 3 http://127.0.0.1:8000/health >/dev/null 2>&1; then
      echo "[start-tp8] 就绪,耗时 ${t}s"; return 0
    fi
    # 引擎若已退出(启动失败),立刻返回失败并把报错打出来
    if ! pgrep -f "vllm serve" >/dev/null 2>&1; then
      echo "[start-tp8] 引擎进程已退出 —— 启动失败。日志尾部:"
      tail -n 40 "$LOG"; return 1
    fi
    sleep 10; t=$((t + 10))
    [ $((t % 60)) -eq 0 ] && echo "  ... 等待中 ${t}s"
  done
  echo "[start-tp8] 超时 3600s 未就绪"; tail -n 40 "$LOG"; return 1
}

cmd_down() {
  pkill -f "start-tp8.sh up" 2>/dev/null
  pkill -TERM -f "vllm serve" 2>/dev/null
  sleep 20
  pkill -KILL -f "vllm serve" 2>/dev/null
  pkill -KILL -f "VLLM::" 2>/dev/null
  sleep 5
  echo "[start-tp8] 停后残留 VLLM:: 进程数=$(ps -eo cmd | grep -c '[V]LLM::')"
}

case "${1:-}" in
  up)    cmd_up ;;
  ready) cmd_ready ;;
  down)  cmd_down ;;
  *)     echo "用法: $0 {up|ready|down}"; exit 2 ;;
esac
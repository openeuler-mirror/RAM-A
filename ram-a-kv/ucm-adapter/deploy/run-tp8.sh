#!/usr/bin/env bash
# Copyright (c) 2026 Huawei Technologies Co.,Ltd. All rights reserved.
#
# RAM-A is licensed under Mulan PSL v2.
# You can use this software according to the terms and conditions of the Mulan
# PSL v2.
# You may obtain a copy of Mulan PSL v2 at:
#         http://license.coscl.org.cn/MulanPSL2
# THIS SOFTWARE IS PROVIDED ON AN "AS IS" BASIS, WITHOUT WARRANTIES OF ANY
# KIND, EITHER EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO
# NON-INFRINGEMENT, MERCHANTABILITY OR FIT FOR A PARTICULAR PURPOSE.
# See the Mulan PSL v2 for more details.

# ==========================================================================
# TP8/DP1 口径:**4 卡(8 die)/ TP8 / DP1** 的 UCM 预取收益测试
#
#   - 原始产物 -> <本目录>/data/{t1,t2,t3}/
#   - 控制台日志/耗时表 -> /mnt/sdb/ucm/real-test/logs/
#
# DP=1 的适配(TP8/DP1 只有一个 rank / 一个 KV 池)由环境变量注入给测试脚本:
#   UCM_DP_SIZE=1            preflight 只要求 /metrics 覆盖 rank 0
#   UCM_HBM_POOL_TOKENS=558665   TP8/DP1 单 rank KV 池(util 0.9、512k 上下文)
#   UCM_METRICS_URL=http://127.0.0.1:8000/metrics  引擎直连
#
# 前置条件:
#   - 引擎(TP8/DP1,见 ./start-tp8.sh)与 router(:9003) 已起
#   - 预放池 /mnt/sdb/ucm/flush_pool_64g.json 有效(replay hbm>0)
#
# 规模档(默认 -medium,与 run.sh 同表):
#   -short   三档各 1 轮,档3 只测最后一轮
#   -medium  三档各 2 轮,档3 测第 3、4 轮(默认)
#   -long    档1×5 / 档2×7 / 档3×6 轮
# ==========================================================================
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)      # ucm-adapter/deploy
ROOT=$(dirname "$HERE")                  # ucm-adapter

if [ -d "$ROOT/scripts" ]; then SC="$ROOT/scripts"; else SC="$HERE/scripts"; fi
DATA="$HERE/data"
LOGS=/mnt/sdb/ucm/real-test/logs

DRYRUN="${DRYRUN:-}"
PROFILE=""
TIERS=""

usage() {
  cat <<'EOF'
用法:
  ./run-tp8.sh [-short|-medium|-long] [-t1|-t2|-t3]...

规模档(默认 -medium,口径同 scripts/run.sh):
  -short    冒烟:三档各 1 轮,档3 只测最后一轮
  -medium   常规:三档各 2 轮,档3 测第 3、4 轮
  -long     交付:档1×5 轮 / 档2×7 轮 / 档3×6 轮

档位选择(可选,默认三档全跑):
  -t1 / -t2 / -t3   可组合,如: ./run-tp8.sh -medium -t2 -t3

产物:
  <本目录>/data/t{1,2,3}/         每轮原始 json / jsonl / md
  /mnt/sdb/ucm/real-test/logs/    <批次>_t*_run*.log + <批次>_timings.tsv

环境变量(有默认,通常不用改):
  UCM_CACHE_BUFFER_CAPACITY_GB  节点级 DDR 缓存总量(GB,默认 64)
  UCM_DP_SIZE                   引擎 DP 路数(TP8/DP1 时为 1)
  UCM_HBM_POOL_TOKENS           每 rank KV 池 token 数(TP8/DP1 为 558665)
  UCM_METRICS_URL               metrics 落点(默认引擎直连 8000)
  UCM_FLUSH_CONCURRENCY         T1 flush 重放并发(默认 2)
  UCM_PRESSURE_CONCURRENCY      T2/T3 压力重放并发(默认 8)
  DRYRUN=1                      只打印将执行的命令,不真跑

压力并发为何默认 8
  压力要能把 HBM 池里的缓存块复用掉(即挤出),「同时在飞的 KV 块数」得顶得上池子:
    - TP4/DP4:池 16,275 块、压力单条 2,003 块,每趟 12 条同时在飞
      → 远超池子,观测 running=9 / usage=0.976,挤出成功。
    - TP8/DP1:池只有 4,364 块(TP4/DP4 的 1/4),压力单条仍是 2,003 块 ≈ 半个池子。
      取 8 → 每趟在飞 16,024 块 ≈ 3.7x 池,池子能被占满
      (引擎日志观测 GPU KV cache usage 到过 98.3%)。
  注意:并发只是必要条件。DP=1 下真正会把冷基准毁掉的是「落盘探针」
  (随机 sid 在单卡上必然落在被测卡,把马上要测的前缀重新灌回 HBM),
  已在 scripts/verify_quick_gain.py / verify_agent_scenario.py 的 persist_probe 里
  对 dp_size<=1 跳过。
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    -short|--short) PROFILE=short ;;
    -medium|--medium) PROFILE=medium ;;
    -long|--long) PROFILE=long ;;
    -t1|--t1) TIERS="$TIERS t1" ;;
    -t2|--t2) TIERS="$TIERS t2" ;;
    -t3|--t3) TIERS="$TIERS t3" ;;
    *) echo "未知参数: $1" >&2; echo >&2; usage >&2; exit 2 ;;
  esac
  shift
done
PROFILE="${PROFILE:-medium}"
TIERS="${TIERS:- t1 t2 t3}"

case "$PROFILE" in
  short)  T1_REP=1; T1_PRE=3; T2_REP=1; T3_REP=1; T3_MEASURE="4" ;;
  medium) T1_REP=2; T1_PRE=3; T2_REP=2; T3_REP=2; T3_MEASURE="3,4" ;;
  long)   T1_REP=5; T1_PRE=5; T2_REP=7; T3_REP=6; T3_MEASURE="3,4,5,6" ;;
esac

POOL=/mnt/sdb/ucm/flush_pool_64g.json
DDR_GB="${UCM_CACHE_BUFFER_CAPACITY_GB:-64}"

# DP=1 口径:注入给三个测试脚本(默认值见各自 --dp-size / --hbm-pool-tokens)
export UCM_DP_SIZE="${UCM_DP_SIZE:-1}"
export UCM_HBM_POOL_TOKENS="${UCM_HBM_POOL_TOKENS:-558665}"
export UCM_METRICS_URL="${UCM_METRICS_URL:-http://127.0.0.1:8000/metrics}"
# 压力并发:池子小,需要更大的「在飞块数/池块数」比(见 usage 说明)
FLUSH_CONC="${UCM_FLUSH_CONCURRENCY:-2}"
PRESSURE_CONC="${UCM_PRESSURE_CONCURRENCY:-8}"

mkdir -p "$LOGS" "$DATA"

SEQ=$(date '+%Y%m%d-%H%M%S')
STAMP0=$(date '+%Y-%m-%d %H:%M:%S')
TIMINGS="$LOGS/${SEQ}_timings.tsv"
if [ -z "$DRYRUN" ]; then
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    ts_done phase run_index elapsed_s rc profile > "$TIMINGS"
fi

stamp() { date '+%Y-%m-%d %H:%M:%S'; }

run() {
  local phase="$1" idx="$2" log="$3"; shift 3
  echo "    $ $*"
  if [ -n "$DRYRUN" ]; then return 0; fi
  local t0 t1 rc el
  t0=$(date +%s)
  "$@" > "$log" 2>&1
  rc=$?
  t1=$(date +%s)
  el=$((t1 - t0))
  echo "    [耗时 $((el / 60)) 分 $((el % 60)) 秒]"
  printf '%s\t%s\t%s\t%d\t%d\t%s\n' \
    "$(stamp)" "$phase" "$idx" "$el" "$rc" "$PROFILE" >> "$TIMINGS"
  [ "$rc" -eq 0 ] || echo "    [warn] exit=$rc(见 $log)"
  return 0
}

echo "[$STAMP0] [TP8/DP1] 规模档 -$PROFILE (档1×$T1_REP / 档2×$T2_REP / 档3×$T3_REP 轮," \
     "档3 测第 $T3_MEASURE 轮) DDR=${DDR_GB}GB"
echo "[$STAMP0] DP_SIZE=$UCM_DP_SIZE  HBM_POOL_TOKENS=$UCM_HBM_POOL_TOKENS  METRICS=$UCM_METRICS_URL"
echo "[$STAMP0] 产物: $DATA/{t1,t2,t3}/  日志: $LOGS/"
[ -n "$DRYRUN" ] && echo "[$STAMP0] DRYRUN=1:只打印命令,不执行"

if echo "$TIERS" | grep -qw t1; then
  for i in $(seq 1 "$T1_REP"); do
    log="$LOGS/${SEQ}_t1_run$i.log"
    echo "[$(stamp)] ===== T1 基础收益 第 $i/$T1_REP 次 -> $log"
    run t1 "$i" "$log" python3 -u "$SC/verify_prefetch_effect_fast.py" \
      --prompt-tokens-list 8192,32768,65536,131072 \
      --prefixes "$T1_PRE" --warmup-prefixes 4 --warmup-tokens 8192 \
      --flush-gb 24 --flush-concurrency "$FLUSH_CONC" \
      --cache-buffer-capacity-gb "$DDR_GB" \
      --pool-file "$POOL" \
      --log-dir "$LOGS" --out-dir "$DATA/t1"
  done
fi

if echo "$TIERS" | grep -qw t2; then
  for i in $(seq 1 "$T2_REP"); do
    log="$LOGS/${SEQ}_t2_run$i.log"
    echo "[$(stamp)] ===== T2 小规模(单卡 6 会话)第 $i/$T2_REP 次 -> $log"
    run t2 "$i" "$log" python3 -u "$SC/verify_quick_gain.py" \
      --sessions 6 --turns 3 --pin-rank 0 \
      --measure-turns 3 --pressure-turns 3 --pressure-passes 3 \
      --pressure-concurrency "$PRESSURE_CONC" \
      --cache-buffer-capacity-gb "$DDR_GB" \
      --pool-file "$POOL" \
      --dp-size "$UCM_DP_SIZE" --hbm-pool-tokens "$UCM_HBM_POOL_TOKENS" \
      --log-dir "$LOGS" --out-dir "$DATA/t2"
  done
fi

if echo "$TIERS" | grep -qw t3; then
  for i in $(seq 1 "$T3_REP"); do
    log="$LOGS/${SEQ}_t3_run$i.log"
    echo "[$(stamp)] ===== T3 大规模(整机 12 会话)第 $i/$T3_REP 次 -> $log"
    run t3 "$i" "$log" python3 -u "$SC/verify_agent_scenario.py" \
      --sessions 12 --turns 6 \
      --measure-turns "$T3_MEASURE" --pressure-turns "$T3_MEASURE" \
      --pressure-passes 4 \
      --pressure-concurrency "$PRESSURE_CONC" \
      --cache-buffer-capacity-gb "$DDR_GB" \
      --pool-file "$POOL" \
      --dp-size "$UCM_DP_SIZE" --hbm-pool-tokens "$UCM_HBM_POOL_TOKENS" \
      --log-dir "$LOGS" --out-dir "$DATA/t3"
  done
fi

echo "[$(stamp)] 全部完成。原始产物: $DATA/{t1,t2,t3}/ ,日志: $LOGS/"

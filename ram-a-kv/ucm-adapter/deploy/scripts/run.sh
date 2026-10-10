#!/usr/bin/env bash
# ==========================================================================
# 一键跑 UCM 预取收益测试(真实流量)
#
#   T1 基础收益:单请求、无并发 —— 同一份 KV「从 SSD 冷读 vs 预热后在内存」
#   T2 小规模  :1 张卡上 6 个 agent 会话同时提问 —— 单卡视角的端到端首字延迟
#   T3 大规模  :4 张卡上 12 个会话 × 6 轮 —— 全集群视角(回源跨卡争抢)
#
# 规模档- 详见 ./run.sh -h
#   -short   三档各 1 轮,档3 只测最后一轮
#   -medium  三档各 2 轮,档3 测第 3、4 轮(默认)
#   -long    档1×5 / 档2×7 / 档3×6 轮
#
# 产物(原始数据落在 reports/data/,清空后重跑即重建)
#   reports/data/t{1,2,3}/            每轮的原始 json / jsonl / md
#   reports/data/logs/<批次>_*.log    每条的完整控制台输出 + <批次>_timings.tsv 耗时表
# ==========================================================================
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(dirname "$HERE")
DATA="$ROOT/reports/data"
LOGS="$DATA/logs"

DRYRUN="${DRYRUN:-}"      # DRYRUN=1 只打印将执行的命令,不真跑(自检用)
PROFILE=""
TIERS=""

usage() {
  cat <<'EOF'
用法:
  ./run.sh [-short|-medium|-long] [-t1|-t2|-t3]...

规模档(默认 -medium):
  -short    冒烟:三档各 1 轮,档3 只测最后一轮
  -medium   常规:三档各 2 轮,档3 测第 3、4 轮
  -long     完整:档1×5 轮 / 档2×7 轮 / 档3×6 轮

  三档的会话数/卡数/上下文长度**不随档位变化**(档2 固定 6 会话单卡,档3 固定
  12 会话 4 卡、6 轮),只有「跑几轮、测哪几轮」不同 —— 这样不同档位的数据口径
  一致、结论可比。耗时随机器与 DDR 容量而变。

档位选择(可选,默认三档全跑):
  -t1   只跑档1(单请求:KV 在盘 vs 在内存)
  -t2   只跑档2(单卡 6 会话 A/B)
  -t3   只跑档3(整机 4 卡 12 会话 A/B)
  可组合,如: ./run.sh -medium -t2 -t3

其它:
  -h, --help              显示本用法
  DRYRUN=1 ./run.sh ...   只打印将要执行的命令,不真跑(自检用)

产物:
  reports/data/t{1,2,3}/        每轮的原始 json / jsonl / md
  reports/data/logs/            每条的完整控制台输出 + 耗时表
  reports/data/charts/*.svg     单独图表

  清空 reports/data/ 后重跑即按新数据重建。

前置条件:
  - vllm 引擎 :8000 已起;router :9003 已起
  - 预放池存在且有效(verify_prefetch_effect_fast.py --preflight)

环境变量:
  UCM_CACHE_BUFFER_CAPACITY_GB  节点级 DDR 缓存总量(GB,默认 64)。
                                与容量挂钩的参数(flush 趟数 / 池组数 / 压力素材条数)
                                由脚本按它推导,不写死 —— 换部署只需设这个变量。
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

# 规模档 -> 轮数 / 采样 / 测点。只改「跑多少」,不改「测什么」。
case "$PROFILE" in
  short)  T1_REP=1; T1_PRE=3; T2_REP=1; T3_REP=1; T3_MEASURE="4" ;;
  medium) T1_REP=2; T1_PRE=3; T2_REP=2; T3_REP=2; T3_MEASURE="3,4" ;;
  long)   T1_REP=5; T1_PRE=5; T2_REP=7; T3_REP=6; T3_MEASURE="3,4,5,6" ;;
esac

POOL=/mnt/sdb/ucm/flush_pool_64g.json
POOL_T3="${POOL}"

# DDR 缓存总量(节点级,GB)。随部署不同 —— 与容量挂钩的参数(flush 趟数、池组数、
# 压力素材条数)不在这里写死,全交给脚本按它推导。改部署只需:
#   export UCM_CACHE_BUFFER_CAPACITY_GB=<值>   然后重跑(默认 64)
DDR_GB="${UCM_CACHE_BUFFER_CAPACITY_GB:-64}"

mkdir -p "$LOGS"

# 混档提醒:不同档位的轮数/测点不同,混在一起的统计口径不一致。
prev=""
[ -f "$DATA/.profile" ] && prev=$(cat "$DATA/.profile" 2>/dev/null)
n_prev=$(find "$DATA" -maxdepth 2 \( -name 'tier_gain_*.json' \
          -o -name 'agent_scenario_*.json' \) 2>/dev/null | wc -l | tr -d ' ')
if [ -n "$prev" ] && [ "$prev" != "$PROFILE" ] && [ "$n_prev" -gt 0 ]; then
  echo "[warn] reports/data/ 里已有 $n_prev 份产物,是 -$prev 档跑的;本次按 -$PROFILE。"
  echo "       两档的轮数/测点不同,混在同一次统计里口径会不一致。建议先清空再跑:"
  echo "         rm -rf $DATA"
fi
[ -n "$DRYRUN" ] || printf '%s\n' "$PROFILE" > "$DATA/.profile"

# 每次执行都带一个批次号(启动时刻),日志与耗时表都以它开头 ——
# 这样反复跑不会互相覆盖(早期版本按 run1/run2 命名,重跑会冲掉上一次的日志)。
SEQ=$(date '+%Y%m%d-%H%M%S')
STAMP0=$(date '+%Y-%m-%d %H:%M:%S')

# 每次执行的耗时单独记一张表(报告第 8 节会自动读最新的一张展示)。
TIMINGS="$LOGS/${SEQ}_timings.tsv"
if [ -z "$DRYRUN" ]; then
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    ts_done phase run_index elapsed_s rc profile > "$TIMINGS"
fi

stamp() { date '+%Y-%m-%d %H:%M:%S'; }

# 跑一条命令:输出写进独立日志文件(文件名含批次号,覆盖写);失败不中断整个序列
# (允许个别轮失败)。每次记录起止时间并把耗时写入 $TIMINGS(时间是该次执行结束的时刻)。
run() {
  local phase="$1" idx="$2" log="$3"; shift 3
  echo "    $ $*"
  if [ -n "$DRYRUN" ]; then
    return 0
  fi
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

echo "[$STAMP0] 规模档 -$PROFILE (档1×$T1_REP / 档2×$T2_REP / 档3×$T3_REP 轮," \
     "档3 测第 $T3_MEASURE 轮) DDR=${DDR_GB}GB"
[ -n "$DRYRUN" ] && echo "[$STAMP0] DRYRUN=1:只打印命令,不执行"

if echo "$TIERS" | grep -qw t1; then
  for i in $(seq 1 "$T1_REP"); do
    log="$LOGS/${SEQ}_t1_run$i.log"
    echo "[$(stamp)] ===== T1 基础收益 第 $i/$T1_REP 次 -> $log"
    run t1 "$i" "$log" python3 -u "$HERE/verify_prefetch_effect_fast.py" \
      --prompt-tokens-list 8192,32768,65536,131072 \
      --prefixes "$T1_PRE" --warmup-prefixes 4 --warmup-tokens 8192 \
      --flush-gb 24 --flush-concurrency 4 \
      --cache-buffer-capacity-gb "$DDR_GB" \
      --pool-file "$POOL"
    # 注:--flush-passes / --pool-groups 不写死,按 --cache-buffer-capacity-gb 推导
    #     (池组数不够时会 [FAIL] 并给出重新 --prepare-pool 的命令)
  done
fi

if echo "$TIERS" | grep -qw t2; then
  for i in $(seq 1 "$T2_REP"); do
    log="$LOGS/${SEQ}_t2_run$i.log"
    echo "[$(stamp)] ===== T2 小规模(单卡 6 会话)第 $i/$T2_REP 次 -> $log"
    run t2 "$i" "$log" python3 -u "$HERE/verify_quick_gain.py" \
      --sessions 6 --turns 3 --pin-rank 0 \
      --measure-turns 3 --pressure-turns 3 --pressure-passes 3 \
      --cache-buffer-capacity-gb "$DDR_GB" \
      --pool-file "$POOL"
  done
fi

if echo "$TIERS" | grep -qw t3; then
  for i in $(seq 1 "$T3_REP"); do
    log="$LOGS/${SEQ}_t3_run$i.log"
    echo "[$(stamp)] ===== T3 大规模(全集群 12 会话)第 $i/$T3_REP 次 -> $log"
    run t3 "$i" "$log" python3 -u "$HERE/verify_agent_scenario.py" \
      --sessions 12 --turns 6 \
      --measure-turns "$T3_MEASURE" --pressure-turns "$T3_MEASURE" \
      --pressure-passes 4 \
      --cache-buffer-capacity-gb "$DDR_GB" \
      --pool-file "$POOL_T3"
  done
fi

echo "[$(stamp)] 全部完成。原始产物: $DATA/{t1,t2,t3}/ ,日志: $LOGS/"
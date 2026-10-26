#!/bin/bash
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

# Real-model verification: DeepAgents + RamAKvMiddleware -> RAM-A @6990, model @8000.
# Traffic log (deepagents-side events + middleware<->RAM-A requests/responses):
#   default: <this script's dir>/ram-a-traffic.log
#   override: RAM_A_TRAFFIC_LOG=/other/path.log bash run_verify_real.sh
#   disable: RAM_A_TRAFFIC_LOG= bash run_verify_real.sh  (set to empty)
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname -- "$0")" && pwd)

S=/home/xxxxx/ram-a-sdk-main
[ -x "$S/.venv/bin/python" ] || { echo "[FAIL] venv missing, run: cd $S && python3 -m venv .venv && . .venv/bin/activate && pip install -r requirements.txt && pip install '.[deepagents]'"; exit 1; }
export MODEL_BASE_URL=http://127.0.0.1:8000/v1
export RAM_A_URL=http://127.0.0.1:6990
export NO_PROXY=127.0.0.1,localhost

# Traffic log location: default alongside this script.
if [ -n "${RAM_A_TRAFFIC_LOG+x}" ]; then
    :  # explicit value (including empty = disabled) from the environment wins
else
    RAM_A_TRAFFIC_LOG="$SCRIPT_DIR/ram-a-traffic.log"
fi
if [ -n "$RAM_A_TRAFFIC_LOG" ]; then
    export RAM_A_TRAFFIC_LOG
    : > "$RAM_A_TRAFFIC_LOG"   # truncate per run; remove this line to append
    echo "[traffic-log] $RAM_A_TRAFFIC_LOG"
else
    unset RAM_A_TRAFFIC_LOG
    echo "[traffic-log] disabled"
fi

exec "$S/.venv/bin/python" /home/xxxxxx/verify_real_model.py "$@"

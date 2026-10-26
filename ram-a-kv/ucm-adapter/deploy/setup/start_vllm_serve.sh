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

# 启动 vllm serve（UCM 预取链路,容器 + 本仓库代码）。
# 必须在容器内执行；cd / 是必需的，否则 cwd 下的 vllm/ 目录会被解析为命名空间包。
set -uo pipefail

export HCCL_BUFFSIZE=1024
export VLLM_ENGINE_READY_TIMEOUT_S=7200

LOG=${VLLM_LOG:-/mnt/sdb/ucm/logs/vllm-rama.log}
MODEL=${VLLM_MODEL_PATH:-/mnt/sdb/models/DeepSeek-V4-Flash-0731-w8a8}
UCM_CONFIG=${UCM_CONFIG_FILE:-/mnt/sdb/ucm/ucm-dsv4.yaml}
cd /

nohup vllm serve "$MODEL" \
  --host 0.0.0.0 --served-model-name deepseek \
  --tokenizer-mode deepseek_v4 --reasoning-parser deepseek_v4 \
  --tool-call-parser deepseek_v4 --enable-auto-tool-choice \
  --max-model-len 1048576 --quantization ascend \
  --tensor-parallel-size 4 --data-parallel-size 4 --enable-expert-parallel \
  --block-size 32 --gpu-memory-utilization 0.9 \
  --max-num-batched-tokens 10240 --max-num-seqs 64 \
  --speculative-config '{"method": "dspark", "num_speculative_tokens": 3, "enforce_eager": true}' \
  --model-loader-extra-config '{"enable_multithread_load": true, "num_threads": 128}' \
  --additional-config '{"ascend_compilation_config": {"enable_npugraph_ex": true, "enable_static_kernel": false}, "enable_cpu_binding": false, "enable_dsa_cp": true, "enable_flashcomm1": true, "multistream_overlap_shared_expert": true}' \
  --kv-transfer-config "{\"kv_connector\": \"UCMConnector\", \"kv_connector_module_path\": \"ucm.integration.vllm.ucm_connector\", \"kv_role\": \"kv_both\", \"kv_connector_extra_config\": {\"UCM_CONFIG_FILE\": \"$UCM_CONFIG\"}}" \
  > "$LOG" 2>&1 &

echo "vllm serve started, pid=$! log=$LOG"
wait

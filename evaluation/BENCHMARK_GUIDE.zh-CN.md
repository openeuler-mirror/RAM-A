# RAM-A Benchmark 操作指南

本文说明如何准备环境、配置数据集、运行 smoke/full benchmark，以及查看结果。命令默认从仓库根目录执行。

## 1. 流程和运行模式

三个数据集由各自 runner 负责数据适配和评测，统一入口负责读取 TOML 并传递 embedding、检索、图记忆、rerank 和回答参数。

一次完整运行的链路是：

```text
adapter -> raw prepared -> 可选原子记忆抽取/grounding -> add
       -> 向量/BM25 索引与图构建 -> search -> rerank
       -> answer -> judge -> retrieval/QA 指标和报告
```

| 配置 | 含义 |
|---|---|
| `execution = "ab"` | 运行 raw 和 extracted 两个 arm，比较原始记忆与原子记忆 |
| `execution = "single"` | 只运行一个 arm，必须设置 `memory_mode = "raw"` 或 `"extracted"` |
| `mode = "normal"` | 普通可复现实验，不做 promotion 晋级判断 |
| `mode = "strict"` | 严格 A/B 晋级实验，必须提供 promotion policy |
| `phase = "full"` | 当前统一入口支持的正式全量模式 |

`ab` 不是 graph/no-graph 或 rerank/no-rerank 对比。它比较 raw 和 extracted，两个 arm 使用同一套 graph、retrieval 和 rerank 配置。研究图或 rerank 增量时，应复制配置，只改变对应开关。

## 2. 创建 Python 环境

```bash
python3 -m venv evaluation/.venv
source evaluation/.venv/bin/activate
python -m pip install --upgrade pip
python -m pip install -r evaluation/requirements.txt
cargo build
```

检查工具：

```bash
python --version
cargo --version
```

如果系统没有 Rust/Cargo，请先安装 Rust stable。

## 3. 设置 API key

API key 只放在环境变量中，不写入 TOML、代码或 Git。默认配置使用 OpenRouter：

```bash
export OPENROUTER_API_KEY="替换为真实密钥"
test -n "$OPENROUTER_API_KEY" || { echo "OPENROUTER_API_KEY 未设置"; exit 1; }
```

当前默认配置中 embedding、图抽取、rerank、answer 和 judge 都使用这个环境变量。更换 provider 时，同时修改 TOML 中相应的 `*_api_key_env` 和 `*_base_url`。

## 4. 准备数据集

完整数据集只保存在本地，不提交到 Git。统一配置使用以下环境变量：

| 数据集 | 环境变量 | 文件 |
|---|---|---|
| LoCoMo | `LOCOMO_DATASET` | `locomo10.json` |
| PersonaMem | `PERSONALMEM_DATASET` | prepared PersonaMem JSON |
| LongMemEval | `LONGMEMEVAL_DATASET` | `longmemeval_oracle.json` 或 runner 支持的 prepared/oracle JSON |

```bash
export LOCOMO_DATASET="/absolute/path/to/locomo10.json"
export PERSONALMEM_DATASET="/absolute/path/to/personalmem-prepared.json"
export LONGMEMEVAL_DATASET="/absolute/path/to/longmemeval_oracle.json"

test -f "$LOCOMO_DATASET"
test -f "$PERSONALMEM_DATASET"
test -f "$LONGMEMEVAL_DATASET"
```

只跑一个数据集时至少设置对应变量；建议三个变量都设置，避免切换数据集时遗漏。

## 5. 配置文件

默认模板是 `evaluation/configs/benchmark-full.toml`。通常只需要修改数据集路径和 provider 环境变量：

```toml
[dataset.locomo]
file = "${LOCOMO_DATASET}"

[dataset.personalmem]
file = "${PERSONALMEM_DATASET}"

[dataset.longmemeval]
file = "${LONGMEMEVAL_DATASET}"
```

实验时常改的字段：

| 字段 | 作用 |
|---|---|
| `run.execution` | `ab` 跑 raw/extracted；`single` 跑一个 arm |
| `run.memory_mode` | 仅 `single` 使用，值为 `raw` 或 `extracted` |
| `run.mode` | 普通运行用 `normal`；严格晋级用 `strict` |
| `run.pair_id` | 本次实验标识，建议每次实验使用新值 |
| `graph.enabled` | search 是否启用图检索 |
| `graph.build_enabled` | add 是否构建图记忆 |
| `rerank.enabled` | 是否启用外部 rerank |
| `retrieval.top_k` | 最终返回的记忆数量 |
| `answer.qa_top_k` | 回答阶段使用的检索结果数量 |

公平比较时不要随意修改 embedding 模型、维度、Hybrid 权重、`candidate_k`、`top_k`、answer/judge 模型。API key 不写入配置文件。

## 6. 推荐 full 命令

```bash
source evaluation/.venv/bin/activate
export OPENROUTER_API_KEY="替换为真实密钥"
export LOCOMO_DATASET="/absolute/path/to/locomo10.json"
export PERSONALMEM_DATASET="/absolute/path/to/personalmem-prepared.json"
export LONGMEMEVAL_DATASET="/absolute/path/to/longmemeval_oracle.json"
```

运行 LoCoMo：

```bash
PYTHONPATH=evaluation \
python evaluation/run_benchmark.py \
  --config evaluation/configs/benchmark-full.toml \
  --dataset locomo
```

运行其他数据集时，将最后的 `locomo` 替换为 `personalmem` 或 `longmemeval`。

默认配置实际启用：`phase=full`、`mode=normal`、`execution=ab`、Hybrid（0.7/0.3）、`candidate_k=150`、`top_k=30`、图构建、图检索、图权重 0.2、外部 rerank（`cohere/rerank-v3.5`，`input_k=40`），以及最多 3 条图事实进入答案上下文。

默认输出目录：

```text
outputs/memory-ab/<dataset>/full/graph-full-v1/
```

## 7. 只运行一个 arm

复制 `benchmark-full.toml`，将 `[run]` 改为：

```toml
[run]
phase = "full"
mode = "normal"
execution = "single"
memory_mode = "extracted"
pair_id = "locomo-extracted-full-v1"
output_root = "../../outputs/memory-single"
```

然后将 `--config` 指向新文件。只跑 raw 时把 `memory_mode` 改为 `raw`。`single` 只支持 `normal`。

## 8. smoke 验证

smoke 只验证代码链路、图构建、参数组合和输出格式，不用于正式分数。建议使用仓库 fixture 和 `hash` embedding，避免远程 embedding 成本。LoCoMo 示例：

```bash
export OPENROUTER_API_KEY="替换为真实密钥"
export MEMORY_BENCH_GRAPH=1
export GRAPH_RERANK=0
export GRAPH_ALLOW_GRAPH_ONLY=0
export MAX_GRAPH_CONTEXT_FACTS=3
export GRAPH_BUILD_CONCURRENCY=1
export PHASE=full
export DATASET="$PWD/evaluation/fixtures/locomo_sample.json"
export RUN_DIR="$PWD/outputs/_smoke/locomo"
bash evaluation/run_locomo_memory_ab.sh
```

PersonaMem 和 LongMemEval 的 fixture smoke 命令见对应 README。fixture 不能替代真实数据集 full，也不能用于判断图记忆是否提升性能。

## 9. normal、strict 和 promotion policy

普通运行使用 `mode = "normal"`，不配置 `promotion_policy`。严格 A/B 晋级使用：

```toml
[run]
phase = "full"
mode = "strict"
execution = "ab"
promotion_policy = "${PROMOTION_POLICY}"
```

并设置：

```bash
export PROMOTION_POLICY="/absolute/path/to/promotion-policy.json"
```

strict 必须有 policy；normal 不应提供 policy。strict 不是日常 smoke/full 的前置步骤。

## 10. 查看结果

优先查看 JSON 指标，只有需要分析失败样例时再打开 HTML：

```bash
python3 - <<'PY'
import json
from pathlib import Path

path = Path("outputs/memory-ab/locomo/full/graph-full-v1/raw/qa_metrics.json")
data = json.loads(path.read_text(encoding="utf-8"))
overall = data.get("overall", {})
for key in ("llm_score", "f1_score", "bleu_score"):
    print(f"{key}: {overall.get(key)}")
PY
```

常见文件：`retrieval_metrics.json`（召回）、`qa_metrics.json`（BLEU/F1/LLM score/token/延迟）、`metrics.json`（数据集专用汇总）、`run_manifest.json`（配置和数据 hash）以及 HTML 详细报告。

## 11. 常见问题和提交边界

- `missing API key env ...`：重新在当前 shell 执行 `export`；激活 venv 不会恢复环境变量。
- 数据集不存在：确认 `${LOCOMO_DATASET}` 等变量指向真实文件。
- graph memory space 无法推导：prepared 数据需要对应的 `scope_id` 元数据，不能关闭校验掩盖空间隔离缺失。
- 远程服务 429：客户端会重试；最终失败时使用 runner 支持的 resume 机制，不要把未完成结果当作正式分数。
- 不提交完整数据集、store/SQLite、search results、responses、cache 和大型 HTML。

提交前执行：

```bash
git status --short
git diff --check
```

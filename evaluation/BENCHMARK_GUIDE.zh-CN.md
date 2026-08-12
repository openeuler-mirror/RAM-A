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
| `execution = "ab"` | 一次运行两个版本：`raw`（原始记忆）和 `extracted`（原子记忆），用于比较两种记忆准备方式。两边共用同一套 graph、retrieval 和 rerank 配置。 |
| `execution = "single"` | 只运行一个版本；必须另外设置 `memory_mode = "raw"` 或 `"extracted"`。适合验证某一套固定配置。 |
| `mode = "normal"` | 普通评测：生成结果和指标，不判断是否达到晋级门槛。日常 full/smoke 使用此模式。 |
| `mode = "strict"` | 严格评测：在 A/B 结果上执行 promotion policy；只有已有评审过的 policy 时使用。 |
| `phase = "full"` | 全量评测模式；当前统一入口不需要先运行 pilot。 |

`ab` 只比较 `raw` 和 `extracted`，不能在同一次运行中配置成“无图 vs 有图”或“无 rerank vs 有 rerank”。这类增量实验需要复制配置文件，分别将 `graph.enabled` 或 `rerank.enabled` 设置为不同值，然后用 `single` 各运行一次。

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

只跑一个数据集时只需设置对应变量。`test -f` 只检查文件是否存在，不会读取或上传数据。

## 5. 配置文件

默认模板是 `evaluation/configs/benchmark-full.toml`。文件中的注释说明每个字段的用途、模板值和系统默认值。通常只需要：

1. 设置数据集环境变量；
2. 设置 `OPENROUTER_API_KEY`；
3. 直接运行命令。

不需要为了普通 full 运行修改模型、权重或图参数。

```toml
[dataset.locomo]
file = "${LOCOMO_DATASET}"

[dataset.personalmem]
file = "${PERSONALMEM_DATASET}"

[dataset.longmemeval]
file = "${LONGMEMEVAL_DATASET}"
```

确实要做对照实验时，常用字段如下：

| 字段 | 作用 |
|---|---|
| `run.execution` | `ab` 比较 raw/extracted；`single` 只运行一个版本。默认 `ab`。 |
| `run.memory_mode` | 仅 `single` 使用，值为 `raw` 或 `extracted`；无默认值。 |
| `run.mode` | `normal` 记录指标；`strict` 执行晋级策略。默认 `normal`。 |
| `run.pair_id` | 运行标识；模板默认 `graph-full-v1`，新实验建议改成新名称。 |
| `graph.enabled` | search 是否加入图候选；系统默认关闭，模板开启。 |
| `graph.build_enabled` | add 时是否构建图；系统默认关闭，模板开启。 |
| `rerank.enabled` | 是否调用外部 rerank；系统默认关闭，模板开启。 |
| `retrieval.top_k` | 最终返回数量；系统默认 `10`，模板为 `30`。 |
| `retrieval.candidate_k` | 候选池大小；未设置时为 `max(top_k × 5, 100)`，模板为 `150`。 |
| `answer.qa_top_k` | 传给答案模型的检索结果数量；模板为 `10`。 |
| `graph.max_context_facts` | 答案阶段最多补充的图事实数量；模板为 `3`。 |

公平比较时，A/B 两次运行必须保持 embedding 模型、维度、Hybrid 权重、`candidate_k`、`top_k`、answer/judge 模型一致，只改变要研究的一个开关。API key 只放环境变量，不写入配置文件。

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

## 11. 常见问题

- `missing API key env ...`：重新在当前 shell 执行 `export`；激活 venv 不会恢复环境变量。
- 数据集不存在：确认 `${LOCOMO_DATASET}` 等变量指向真实文件。
- graph memory space 无法推导：prepared 数据需要对应的 `scope_id` 元数据，不能关闭校验掩盖空间隔离缺失。
- 远程服务 429：客户端会重试；最终失败时使用 runner 支持的 resume 机制，不要把未完成结果当作正式分数。
- 429 或网络超时：这是远程 provider 限流或网络问题，不代表图逻辑失败；等待重试结束。最终失败时，保留运行目录并使用 runner 支持的 resume 机制。
- 不要把未完成运行目录中的指标当作正式结果；确认存在最终 metrics JSON 后再记录分数。

完整数据集、store/SQLite、search results、responses、cache 和大型 HTML 只保存在本地，不作为评测命令的必需输入，也不应提交到 Git。

`git status --short` 和 `git diff --check` 是代码贡献者提交前的检查，不是普通 benchmark 用户必须执行的步骤。

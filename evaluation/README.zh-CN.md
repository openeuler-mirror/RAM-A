# 评估

**[English](README.md)**

RAM-A 记忆系统的评估流水线总览。这里是使用指南入口；各数据集的完整参数和分阶段命令见对应子目录 README。

## 数据集

| 数据集 | 关注点 | 问题数 | 来源 / 下载 | 本地放置位置 |
|--------|--------|--------|-------------|--------------|
| PersonaMem | 长对话画像下的个性化选择题回答 | 32k 切分 589 题；另有 128k、1M | [GitHub](https://github.com/bowen-upenn/PersonaMem), [HuggingFace](https://huggingface.co/datasets/bowen-upenn/PersonaMem) | `data/personalmem/raw/`，再生成到 `data/personalmem/prepared/` |
| LongMemEval | 多轮会话长期记忆问答 | 500 | [GitHub](https://github.com/xiaowu0162/longmemeval), [cleaned oracle JSON](https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned/resolve/main/longmemeval_oracle.json) | `data/longmemeval/longmemeval_oracle.json` |
| LoCoMo | 超长对话记忆问答 | 约 1,986 题 | [GitHub](https://github.com/snap-research/locomo), [locomo10.json](https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json) | `data/locomo/locomo10.json` |

完整 benchmark 数据只作为本地下载文件使用，不提交到主仓。仓库内仅保留
`evaluation/fixtures/` 下的小型 smoke 数据。

## 环境准备

```bash
python3 -m venv evaluation/.venv
source evaluation/.venv/bin/activate
pip install -r evaluation/requirements.txt
cargo build

export OPENROUTER_API_KEY="..."   # LongMemEval / PersonaMem 默认嵌入与回答
export OPENAI_API_KEY="..."       # LoCoMo 回答与评判；可配合 OPENAI_BASE_URL 指向兼容端点
```

所有命令默认从仓库根目录运行；LoCoMo 的 shell 脚本除外，需要先进入 `evaluation/`。
如果用 OpenRouter 跑 LoCoMo 的回答/评判，可以设置 `OPENAI_API_KEY="$OPENROUTER_API_KEY"` 并设置 `OPENAI_BASE_URL=https://openrouter.ai/api/v1`。

## 其他模型供应商配置

评估里有两类模型，配置入口不同：

| 阶段 | 用途 | 当前配置方式 |
|------|------|--------------|
| 嵌入模型 | add/search，把记忆和问题向量化 | `memory-bench` 当前支持 `--embedding openrouter` 或 `--embedding hash`；可改 `--model`、`--dimensions`、`--api-key-env` |
| 回答/评判模型 | 根据检索结果回答问题，或用 LLM-as-judge 评分 | 使用 OpenAI-compatible chat completions；可改模型名、API key 环境变量和 base URL |

注意：`hash` 只用于冒烟测试，不代表真实语义检索能力。当前 CLI 没有暴露嵌入模型的自定义 base URL；如果要直接接入非 OpenRouter 的嵌入供应商，需要先扩展 `memory-bench`，或通过 OpenRouter 统一转发。

回答/评判模型可以切换到其他 OpenAI-compatible 服务：

```bash
# OpenRouter
export OPENROUTER_API_KEY="..."
# LongMemEval: --llm-api-key-env OPENROUTER_API_KEY --llm-base-url https://openrouter.ai/api/v1
# PersonaMem:  --answer-api-key-env OPENROUTER_API_KEY --answer-base-url https://openrouter.ai/api/v1

# 智谱等 OpenAI-compatible 端点示例
export ZHIPU_API_KEY="..."
# LongMemEval: --answerer-model glm-5 --judge-model glm-5 \
#   --llm-api-key-env ZHIPU_API_KEY \
#   --llm-base-url https://open.bigmodel.cn/api/coding/paas/v4
# PersonaMem: --answer-model glm-5 \
#   --answer-api-key-env ZHIPU_API_KEY \
#   --answer-base-url https://open.bigmodel.cn/api/coding/paas/v4

# LoCoMo 回答生成使用 OpenAI SDK 环境变量；评判使用统一 LLM 参数
export OPENAI_API_KEY="$ZHIPU_API_KEY"
export OPENAI_BASE_URL="https://open.bigmodel.cn/api/coding/paas/v4"
export MODEL="glm-5"
export LLM_API_KEY_ENV="OPENAI_API_KEY"
export LLM_BASE_URL="$OPENAI_BASE_URL"
export JUDGE_MODEL="$MODEL"
```

LoCoMo 的 RAM-A 嵌入阶段仍需要 `OPENROUTER_API_KEY`，除非后续新增其他嵌入后端。

## 快速开始

```bash
# PersonaMem（32k，冒烟测试）
python evaluation/personalmem/run.py official-pipeline \
  --size 32k --limit-questions 5 --embedding hash

# LongMemEval（冒烟测试）
python3 evaluation/longmemeval/run.py \
  --embedding hash --embedding-model hash --dimensions 128 --max-questions 5

# LoCoMo（完整流水线）
cd evaluation && ./run_locomo_eval.sh memory_bench
```

## 有证据约束的记忆预处理（特性 2 和 4）

数据集 adapter 先生成包含原始对话记录的 `benchmark-prepared-v1` 文件。记忆流水线
再把消息组织成 episode，构建“候选区唯一归属、上下文允许重叠”的 extraction window，
抽取原子记忆，校验精确原文引用，并且只写入 grounding 结果为 `SUPPORTED` 的记忆。

先用已有 adapter 生成 raw prepared 文件，例如：

```bash
# PersonaMem（先下载官方数据）
python evaluation/personalmem/run.py prepare \
  --size 32k \
  --schema-version benchmark-prepared-v1 \
  --prepared-dataset outputs/personalmem/raw-prepared.json

# LongMemEval
PYTHONPATH=evaluation python -c \
  'from longmemeval.preprocess import preprocess; preprocess("data/longmemeval/longmemeval_oracle.json", "outputs/longmemeval/raw-prepared.json")'
```

使用任意 OpenAI-compatible chat 端点执行抽取和独立的 grounding 验证：

```bash
cargo run --quiet --manifest-path Cargo.toml -p memory-pipeline -- \
  --input outputs/longmemeval/raw-prepared.json \
  --output outputs/longmemeval/extracted-prepared.json \
  --artifacts-dir outputs/longmemeval/memory-pipeline \
  --model openai/gpt-4o-mini \
  --verifier-model openai/gpt-4o-mini \
  --api-key-env OPENROUTER_API_KEY \
  --cache-dir outputs/longmemeval/memory-pipeline-cache
```

后续把 `extracted-prepared.json` 交给现有 add/search 链路即可。输出记录统一使用
`metadata.memory_kind=extracted_memory`，无需修改 Rust 记忆结构或检索逻辑。制品目录
包含标准化消息、episode、extraction window、原始候选记忆、已接收记忆、拒绝/隔离记录、
token/cache 统计和确定性的运行元数据。episode 与 window 只是组织上下文和审计的单元，
不会直接 embedding 或入库；真正进入索引的只有输出 prepared 文件里的已接收记忆。

离线 fixture 模式用两个 JSON 映射替代模型调用：

```bash
cargo run --quiet --manifest-path Cargo.toml -p memory-pipeline -- \
  --input outputs/longmemeval/raw-prepared.json \
  --output /tmp/extracted-prepared.json \
  --artifacts-dir /tmp/memory-pipeline \
  --extractor-responses /tmp/extraction-responses.json \
  --grounding-responses /tmp/grounding-responses.json
```

抽取响应映射以确定性的 window ID 为 key，value 是 `atomic_memory_v1` 响应对象；grounding
映射以确定性的候选记忆 ID 为 key，value 是状态字符串或 `{status, reason}` 对象。两个文件
必须同时提供，CLI 不会写入未经验证的记忆。在线模式会为每个未命中缓存的 window 调用一次
抽取模型，并为含有合法候选记忆的 window 调用一次验证模型；评估成本前应先查看
`extraction_stats.json`。

## 图记忆模式

`memory-bench` 当前可以直接运行 graph-enabled add/search：

- add：传入 `--graph-build`，在普通 MemoryRecord add 成功后继续构建图记忆；
- search：传入 `--graph`，在 `MemoryManager::search(...)` 中开启 graph retrieval channel；
- 图候选抽取使用 OpenAI-compatible chat-completions 端点。默认配置为
  `--graph-llm-api-key-env OPENROUTER_API_KEY`、
  `--graph-llm-base-url https://openrouter.ai/api/v1`、
  `--graph-llm-model openai/gpt-4o-mini`。

在 graph `auto` memory-space 模式下，prepared schema 查询使用
`--graph-memory-space-field` 指定的 filter 字段（默认 `scope_id`），raw top-level-array
数据使用 `path:$[0]` 这类 path space。单条 `--query` 搜索需要传
`--filter '{"scope_id":"..."}'`，或者显式指定 memory-space 模式。`--resume --graph-build`
时，已有 MemoryRecord 不等于 graph 构建完成：已 completed 的 graph run 会跳过，缺失的
graph run 会补构，failed/running 的 graph run 会明确报错。

示例：

```bash
export OPENROUTER_API_KEY="..."

cargo run -p memory-bench -- \
  --store data/locomo_graph.sqlite \
  --embedding openrouter \
  --model baai/bge-m3 \
  --dimensions 1024 \
  --graph-build \
  add \
  --dataset data/locomo/locomo10.json \
  --text-fields text,content,message,memory

cargo run -p memory-bench -- \
  --store data/locomo_graph.sqlite \
  --embedding openrouter \
  --model baai/bge-m3 \
  --dimensions 1024 \
  --graph \
  --graph-weight 0.2 \
  search \
  --dataset data/locomo/locomo10.json \
  --query-fields question,query \
  --top-k 10 \
  --output outputs/locomo_graph_top10.json
```

对比分数时建议 baseline 和 graph 使用不同 SQLite 文件。LoCoMo 是图记忆效果分析的重点数据集，
但最终报告前 LongMemEval 和 PersonaMem 也需要做全量端到端验证。各数据集 wrapper 的 graph
参数透传会在启用后写入对应数据集 README。

## 输出说明

所有评估默认写入仓库根目录下的 `outputs/<数据集>/<时间戳>/`，包含：

- JSON 指标与原始结果
- HTML 报告供快速查看
- `run_meta.json` 含配置与 git hash

大型原始结果不要提交到 Git。用于版本对比的精简记录放在
`evaluation/baselines/`，完整 raw artifact 上传到对象存储、Release 资产或其他制品系统。

## 受治理的 raw/extracted 配对实验

PersonaMem、LongMemEval、LoCoMo 的晋级实验统一使用下面的入口。Pilot 前必须先写好显式
policy JSON；PersonaMem 和 LongMemEval 使用 `memory-ab-promotion-v1`，指标路径必须是
dotted path，并在 `completeness_counts` 中提前写入所选 full 数据集的权威计数。只有两臂
和 comparison 都成功且 pilot 检查通过后，才会写 frozen manifest。

```bash
cargo build -p memory-pipeline
export MEMORY_PIPELINE_BIN="$PWD/target/debug/memory-pipeline"

PYTHONPATH=evaluation evaluation/.venv/bin/python evaluation/scripts/run_memory_ab.py \
  --dataset personalmem --phase pilot --pair-id personalmem-32k-v1 \
  --dataset-file data/personalmem/prepared/personalmem_32k.json \
  --promotion-policy /absolute/path/personalmem-policy.json

PYTHONPATH=evaluation evaluation/.venv/bin/python evaluation/scripts/run_memory_ab.py \
  --dataset personalmem --phase full --pair-id personalmem-32k-v1 \
  --dataset-file data/personalmem/prepared/personalmem_32k.json \
  --promotion-policy /absolute/path/personalmem-policy.json \
  --frozen-config evaluation/outputs/memory-ab/personalmem/pilot/personalmem-32k-v1/frozen_config.json
```

运行其他 registry 时替换 `--dataset` 和数据文件即可；`--` 后面的参数会原样传给两臂。
任何 arm 启动前，入口会运行并记录 Python suite、Rust workspace test、Clippy 和
`git diff --check`。每个 arm 会自行验证 dataset-bound preflight，并在 `config.json`
中写入其真实 SHA-256。

制品位于 `evaluation/outputs/memory-ab/<dataset>/<phase>/<pair-id>/`，`raw/` 与
`extracted/` 使用独立 store，目录还包含 `preflight.json`、`comparison.json` 和
`comparison.html`。Pilot 和不完整 pair 永不写 history；完整 full pair 才按 raw、
extracted 顺序追加到 `history/records/<dataset>.jsonl` 并重新生成 XLSX。晋级失败的完整
full pair 仍以 failed 状态记录，但不能成为 baseline。在人工用真实 provider 完成受控
full command 之前，不宣称 live 分数或晋级结果。

## 测试

```bash
PYTHONPATH=evaluation python -m pytest evaluation
cargo test
```

## 参考文献

- PersonaMem: *Know Me, Respond to Me* (NeurIPS 2025)
- LongMemEval: *Benchmarking Chat Assistants on Long-Term Interactive Memory* (ICLR 2025)
- LoCoMo: *Evaluating Very Long-Term Conversational Memory of LLM Agents* (ACL 2024)

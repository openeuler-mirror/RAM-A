# PersonaMem 评估

**[English](README.md)**

RAM-A 的 PersonaMem 评估脚本。

## 数据集

[PersonaMem](https://github.com/bowen-upenn/PersonaMem)（COLM 2025 / NeurIPS 2025）评估
LLM 从长对话历史中推断用户画像，并在选择题形式下生成个性化回答的能力。提供三个上下文长度切分：**32k**、**128k**、**1M** token；32k 切分约 589 个问题。

## 环境准备

```bash
pip install -r evaluation/requirements.txt
cargo build
```

## 环境变量

| 变量 | 用途 | 是否必需 |
|------|------|---------|
| `OPENROUTER_API_KEY` | 向量嵌入（默认）、回答模型（默认） | 正式运行时必需 |

## 模型供应商配置

PersonaMem 也分为嵌入链路和回答链路：

- 嵌入链路：`add/search/eval` 通过 `memory-bench` 调用嵌入模型；当前支持 `--embedding openrouter` 或 `--embedding hash`，可改 `--model`、`--dimensions`。正式分数不要使用 `hash`。
- 回答链路：`answer` 子命令使用 OpenAI-compatible chat completions；可通过 `--answer-model`、`--answer-api-key-env`、`--answer-base-url` 切换供应商。

示例：

```bash
# 默认 OpenRouter
export OPENROUTER_API_KEY="..."
python evaluation/personalmem/run.py answer \
  --run-dir "$RUN_DIR" --resume \
  --answer-model openai/gpt-4o-mini \
  --answer-api-key-env OPENROUTER_API_KEY \
  --answer-base-url https://openrouter.ai/api/v1

# 智谱等 OpenAI-compatible 服务
export ZHIPU_API_KEY="..."
python evaluation/personalmem/run.py answer \
  --run-dir "$RUN_DIR" --resume \
  --answer-model glm-5 \
  --answer-api-key-env ZHIPU_API_KEY \
  --answer-base-url https://open.bigmodel.cn/api/coding/paas/v4
```

如果只替换回答模型，已有的检索结果可以复用；如果替换嵌入模型或维度，需要重新运行 add/search/eval。

## 命令

```
python evaluation/personalmem/run.py <命令> [选项]
```

| 命令 | 说明 |
|------|------|
| `download` | 下载官方 PersonaMem CSV/JSONL 文件 |
| `prepare` | 将下载文件转换为统一 JSON 数据集 |
| `add` | 将记忆写入向量存储 |
| `search` | 对每个问题检索记忆 |
| `eval` | 对检索结果评分 |
| `answer` | 基于检索上下文生成模型回答 |
| `grade` | 对回答进行评判并计算准确率 |
| `pipeline` | 运行 add → search → eval |
| `official-pipeline` | 运行 download → prepare → add → search → eval |

`prepare` 始终输出 `schema_version=benchmark-prepared-v1`；
`--schema-version` 仅保留为已弃用的兼容参数，不会再选择 legacy 输出。

## Raw/Extracted 记忆 A/B

`pipeline` 和 `official-pipeline` 支持配对的 `raw` 与 `extracted` arm。两个 arm
保留完全相同的 prepared queries 和不可变检索/回答设置。每个 arm 必须使用不同的
`--run-dir`；各自的 store 独立，同一目录不能跨 memory mode 复用。

没有 memory-mode marker 的既有 store 会被视为 legacy raw store：先显式使用
`--memory-mode raw` 运行一次以完成认领。extracted arm 会拒绝这类 store；请为
extracted memory 使用新的 store 路径。

先准备一次共享 raw 输入，再运行 full pair：

```bash
python evaluation/personalmem/run.py prepare \
  --size 32k \
  --prepared-dataset outputs/personalmem/pair-001/raw_prepared.json

python evaluation/personalmem/run.py pipeline \
  --dataset outputs/personalmem/pair-001/raw_prepared.json \
  --memory-mode raw --phase full --pair-id pair-001 \
  --run-dir outputs/personalmem/pair-001/raw

python evaluation/personalmem/run.py pipeline \
  --dataset outputs/personalmem/pair-001/raw_prepared.json \
  --indexed-dataset outputs/personalmem/pair-001/extracted/extracted_prepared.json \
  --memory-mode extracted --phase full --pair-id pair-001 \
  --run-dir outputs/personalmem/pair-001/extracted \
  --extraction-model openai/gpt-4o-mini \
  --verifier-model openai/gpt-4o-mini
```

extracted arm 将 normalization、window、extraction、grounding、aggregation 和
prepared output 全部交给共享 Rust `memory-pipeline`。Python 只负责 PersonaMem
数据适配与编排，并复用现有 add/search/eval/answer/grade 阶段。

受治理的 full run 还必须同时提供：

```text
--phase full --promotion-policy path/to/promotion-policy.json
```

runner 会在数据集/提取阶段、provider client 构造或 run-dir 写入之前校验运行配置与
promotion-policy hash。后续执行 `answer` 和 `grade` 时，应继续
传入相同的 `--memory-mode`、`--dataset`、`--indexed-dataset`、提取设置和
`--run-dir`。

确定性 CI 路径通过 `evaluation/personalmem/run_test.py` 使用
`evaluation/fixtures/personalmem_memory_*_responses.json` 两个静态响应表、hash
embedding 和小型 PersonaMem fixture，全程不访问网络。这些 fixture 只验证编排；
这里没有运行 live PersonaMem benchmark，也不声称分数提升或晋级。

## 主要参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `--embedding` | `openrouter` | `openrouter` 或 `hash` |
| `--model` | `baai/bge-m3` | 嵌入模型 |
| `--dimensions` | 1024 | 嵌入维度 |
| `--top-k` | 10 | 检索结果数量 |
| `--answer-model` | `openai/gpt-4o-mini` | 回答阶段的聊天模型 |
| `--answer-base-url` | `https://openrouter.ai/api/v1` | OpenAI 兼容 chat completions 地址 |
| `--answer-api-key-env` | `OPENROUTER_API_KEY` | 回答模型 API key 环境变量 |
| `--context-token-budget` | 2000 | 回答 prompt 中检索上下文的最大 token 数（0 = 不限） |
| `--run-dir` | *（自动）* | 输出到 `outputs/personalmem/<时间戳>_<memory-mode>/` |
| `--resume` | false | 跳过已有输出的步骤 |
| `--size` | `32k` | 官方切分（`32k`、`128k`、`1M`） |
| `--limit-questions` | 0 | 限制问题数量用于冒烟测试 |
| `--memory-mode` | `raw` | 索引 raw turn 或 Rust 生成的 extracted memory |
| `--phase` | `full` | benchmark 阶段 |
| `--pair-id` | `standalone` | 两个配对 arm 共用的标识 |
| `--indexed-dataset` | `outputs/personalmem_extracted_prepared.json` | extracted arm 的 Rust prepared 输出 |
| `--promotion-policy` | *（无）* | 晋级策略；strict 模式必需 |
| `--graph-build` | false | add 阶段构建图记忆 |
| `--graph-build-concurrency` | 1 | 同时构建的图记录上限；应根据服务商限流逐步提高 |
| `--graph` | false | search 阶段开启 graph retrieval |
| `--graph-weight` | 0.2 | graph retrieval 融合权重 |
| `--graph-llm-api-key-env` | `OPENROUTER_API_KEY` | 图候选抽取 API key 环境变量 |
| `--graph-llm-model` | `openai/gpt-4o-mini` | 图候选抽取模型 |

完整参数列表：`python evaluation/personalmem/run.py <命令> --help`

## 快速冒烟测试

```bash
python evaluation/personalmem/run.py pipeline \
  --dataset evaluation/fixtures/personalmem_sample.json \
  --embedding hash --top-k 2
```

## 完整运行（32k）

```bash
export OPENROUTER_API_KEY="your-key"

RUN_DIR=outputs/personalmem/$(date +%Y-%m-%dT%H%M%S)
python evaluation/personalmem/run.py official-pipeline \
  --size 32k --top-k 10 --run-dir "$RUN_DIR"

# 获取 QA 准确率。注意：answer/grade 必须复用 official-pipeline 生成的同一个 run_dir。
python evaluation/personalmem/run.py answer --run-dir "$RUN_DIR" --resume
python evaluation/personalmem/run.py grade --run-dir "$RUN_DIR" --resume
```

`official-pipeline` 只运行下载、准备、写入、检索和检索评分；不会自动调用回答模型。需要最终 QA Accuracy 时，再运行 `answer` 和 `grade`。
如果不显式传 `--run-dir`，脚本会自动创建时间戳目录；此时请从终端输出的 `report.html` 路径中确认后续要复用的目录。

开启图记忆检索：

```bash
export OPENROUTER_API_KEY="your_openrouter_key"

RUN_DIR=outputs/personalmem/$(date +%Y-%m-%dT%H%M%S)_graph
python evaluation/personalmem/run.py official-pipeline \
  --size 32k \
  --top-k 10 \
  --run-dir "$RUN_DIR" \
  --embedding openrouter \
  --model baai/bge-m3 \
  --dimensions 1024 \
  --graph-build \
  --graph \
  --graph-llm-model openai/gpt-4o-mini
```

`--graph-build` 在 add 阶段构建图记忆；`--graph` 在 search 阶段开启 graph retrieval。
对比分数时，请让 graph 和非 graph 运行使用不同 `--run-dir` / `--store`。

## 一键式完整运行

v1 shell wrapper 会运行完整 PersonaMem 流程，包括回答生成和最终评分：

```bash
# RAM-A
evaluation/scripts/run_personalmem_ram_a_v1.sh --size 32k --top-k 20

# mem0 local 对照
evaluation/scripts/run_personalmem_mem0_local_v1.sh --size 32k --top-k 20
```

默认产物会写入：

```
outputs/personalmem/personalmem_<size>_v1_<backend>_top<k>_<context>_<answer-model>/
  search_results.json
  retrieval_metrics.json
  responses.json
  grade_metrics.json
  grade_results.csv
  report.html
  errors.html
  run_meta.json
  stage_reports/
```

可以用 `--run-dir` 指定产物目录，用 `--resume` 尽量复用已有 prepared data、store 和 responses。

## 检索评分

当 gold 字符串作为子串出现在检索到的记忆文本中时计为命中。
匹配为单向，避免非常短的检索片段匹配到较长 gold 答案内部造成的误报。

## 输出文件

```
outputs/personalmem/<时间戳>_<memory-mode>/
  store.sqlite             # SQLite hybrid 存储
  extracted_prepared.json  # 仅 extracted arm
  artifacts/               # Rust 提取审计产物
  cache/memory-pipeline/   # extraction/grounding 缓存
  stages/                  # 可恢复 stage 完成清单
  search_results.json      # 原始 top-k 结果
  retrieval_metrics.json   # hit@k、MRR、逐题明细
  responses.json           # 生成的回答
  grade_metrics.json       # 准确率、逐题明细
  grade_results.csv        # CSV 汇总
  report.html              # 主报告（检索 + QA，如已评分）
  errors.html              # 错题详情（如已评分）
  stage_reports/           # 分阶段 HTML，例如 retrieval_metrics.html、grade_metrics.html
  run_meta.json            # 运行元数据
```

## 参考文献

- [PersonaMem GitHub](https://github.com/bowen-upenn/PersonaMem)
- [PersonaMem-v2 (HuggingFace)](https://huggingface.co/datasets/bowen-upenn/PersonaMem-v2)
- 论文：*Know Me, Respond to Me* (NeurIPS 2025)

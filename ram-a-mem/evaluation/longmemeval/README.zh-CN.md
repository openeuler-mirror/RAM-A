# LongMemEval 评估

**[English](README.md)**

RAM-A 的 LongMemEval 评估脚本。

## 数据集

[LongMemEval](https://github.com/xiaowu0162/longmemeval)（ICLR 2025）包含 500 个问题，
评估长期对话记忆中的信息提取、多会话推理、知识更新、时间推理和偏好回忆等能力。当前脚本使用 `longmemeval_oracle.json`，即每题只在对应 oracle 会话范围内检索。

## 环境准备

```bash
pip install -r evaluation/requirements.txt
cargo build

mkdir -p data/longmemeval
wget https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned/resolve/main/longmemeval_oracle.json \
  -O data/longmemeval/longmemeval_oracle.json
```

## 环境变量

| 变量 | 是否必需 |
|------|---------|
| `OPENROUTER_API_KEY` | 正式运行时必需（嵌入 + LLM） |

如果使用其他 OpenAI 兼容服务，可通过 `--api-key-env`、`--llm-api-key-env` 和 `--llm-base-url` 指定。

## 模型供应商配置

LongMemEval 有两个模型链路：

- 嵌入链路：`--embedding openrouter` 调用 OpenRouter embeddings；可改 `--embedding-model`、`--dimensions`、`--api-key-env`。当前 CLI 没有暴露嵌入 base URL，非 OpenRouter 嵌入供应商需要先扩展 `memory-bench`。
- QA 链路：回答模型和评判模型走 OpenAI-compatible chat completions；可用 `--answerer-model`、`--judge-model`、`--llm-api-key-env`、`--llm-base-url` 切换供应商。

示例：

```bash
# OpenRouter（默认推荐）
export OPENROUTER_API_KEY="..."
python3 evaluation/longmemeval/run.py --pipeline-phase all \
  --embedding openrouter --embedding-model baai/bge-m3 --dimensions 1024 \
  --answerer-model openai/gpt-4o-mini \
  --judge-model openai/gpt-4o-mini \
  --llm-api-key-env OPENROUTER_API_KEY \
  --llm-base-url https://openrouter.ai/api/v1

# 智谱等 OpenAI-compatible 服务只替换 QA 链路；嵌入仍按上面的 embedding 参数配置
export ZHIPU_API_KEY="..."
python3 evaluation/longmemeval/run.py --pipeline-phase qa --resume \
  --run-dir outputs/longmemeval/<你的运行目录> \
  --answerer-model glm-5 \
  --judge-model glm-5 \
  --llm-api-key-env ZHIPU_API_KEY \
  --llm-base-url https://open.bigmodel.cn/api/coding/paas/v4 \
  --llm-thinking disabled
```

`--llm-thinking` 是供应商相关参数，主要用于 GLM 这类可能返回 reasoning 内容的模型；OpenRouter/OpenAI 常规模型一般保持 `default` 即可。

## 命令

```bash
python3 evaluation/longmemeval/run.py [选项]
```

### 主要参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `--memory-mode` | `raw` | 索引 `raw` turn 或 Rust 生成的 `extracted` memory |
| `--phase` | `full` | benchmark 阶段 |
| `--pipeline-phase` | `retrieval` | 执行 `retrieval`、`qa` 或 `all` 阶段 |
| `--pair-id` | `standalone` | 配对 arms 共用的稳定标识 |
| `--promotion-policy` | *（无）* | 写入 manifest hash 的晋级策略；`full` 必需 |
| `--backend` | `RAM-A` | RAM-A 后端 key |
| `--embedding` | `openrouter` | `openrouter` 或 `hash` |
| `--embedding-model` | `baai/bge-m3` | 嵌入模型 |
| `--dimensions` | 1024 | 嵌入维度 |
| `--api-key-env` | `OPENROUTER_API_KEY` | 嵌入 API key 环境变量 |
| `--retrieval-top-k` | 10 | 检索 top-k |
| `--embedding-batch-size` | 64 | add/search 嵌入批大小 |
| `--resume` | false | 跳过已完成步骤 |
| `--run-dir` | *（自动）* | 指定输出目录或恢复目录 |
| `--max-questions` | *（全部）* | 冒烟测试限制 |
| **抽取阶段** | | |
| `--extraction-model` | `openai/gpt-4o-mini` | 原子记忆抽取模型 |
| `--verifier-model` | `openai/gpt-4o-mini` | grounding 校验模型 |
| `--extraction-cache-dir` | `<run>/cache/memory-pipeline` | 抽取缓存目录 |
| `--max-candidate-tokens` | 320 | candidate window 预算 |
| `--max-window-tokens` | 640 | candidate 加上下文预算 |
| `--context-before-messages` | 2 | 向前包含的上下文消息数 |
| `--context-after-messages` | 0 | 向后包含的上下文消息数 |
| `--extractor-responses` / `--grounding-responses` | *（无）* | 完全离线抽取使用的成对响应 maps |
| **图记忆** | | |
| `--graph-build` | false | add 阶段构建图记忆 |
| `--graph-build-concurrency` | 1 | 同时构建的图记录上限；应根据服务商限流逐步提高 |
| `--graph` | false | search 阶段开启 graph retrieval |
| `--graph-weight` | 0.2 | graph retrieval 融合权重 |
| `--graph-fail-open` | false | graph search 失败时退化为非 graph 检索 |
| `--graph-memory-space-mode` | `auto` | memory space 推导方式 |
| `--graph-llm-api-key-env` | `OPENROUTER_API_KEY` | 图候选抽取 API key 环境变量 |
| `--graph-llm-model` | `openai/gpt-4o-mini` | 图候选抽取模型 |
| `--graph-llm-base-url` | `https://openrouter.ai/api/v1` | OpenAI 兼容图候选抽取 base URL |
| **QA 阶段** | | |
| `--answerer-model` | `openai/gpt-4o-mini` | 回答生成模型 |
| `--judge-model` | `openai/gpt-4o-mini` | LLM 评判模型 |
| `--llm-api-key-env` | `OPENROUTER_API_KEY` | 回答/评判模型 API key 环境变量 |
| `--llm-base-url` | `https://openrouter.ai/api/v1` | OpenAI 兼容 chat completions 地址 |
| `--qa-top-k` | 10 | QA 使用的记忆数量 |
| `--answer-prompt-version` | `lme_default` | 提示模板版本 |
| `--memory-format` | `full` | `full` 或 `compact` |
| `--show-scores` | false | 是否把检索分数暴露给回答模型 |
| `--qa-output-tag` | *（自动）* | 覆盖 QA 输出文件标签 |
| `--llm-thinking` | `default` | GLM 等模型的 thinking 控制 |

完整参数列表：`python3 evaluation/longmemeval/run.py --help`

## 快速冒烟测试

```bash
python3 evaluation/longmemeval/run.py \
  --embedding hash --embedding-model hash --dimensions 128 --max-questions 5
```

## 完整运行

```bash
export OPENROUTER_API_KEY="your-key"

# 仅检索
python3 evaluation/longmemeval/run.py

# 检索 + QA full run
python3 evaluation/longmemeval/run.py --phase full --pipeline-phase all \
  --answerer-model openai/gpt-4o-mini \
  --judge-model openai/gpt-4o-mini \
  --llm-api-key-env OPENROUTER_API_KEY \
  --llm-base-url https://openrouter.ai/api/v1

# 图记忆检索
python3 evaluation/longmemeval/run.py \
  --graph-build \
  --graph \
  --graph-llm-model openai/gpt-4o-mini
```

受治理的 `--phase full` 还必须提供 `--promotion-policy`。runner 会在预处理或构造
embedding/chat client 之前校验 immutable 字段与策略 hash。

## 完全离线的 extracted fixture

下面的 smoke run 使用仓库内成对响应 maps 和 hash embedding，不需要 API key，也不代表
正式 benchmark 分数。

```bash
python3 evaluation/longmemeval/run.py \
  --dataset-file "$PWD/evaluation/fixtures/longmemeval_sample.json" \
  --run-dir /tmp/longmemeval-extracted-offline \
  --memory-mode extracted --phase full --pipeline-phase retrieval \
  --pair-id offline-longmemeval \
  --extractor-responses "$PWD/evaluation/fixtures/longmemeval_memory_extractor_responses.json" \
  --grounding-responses "$PWD/evaluation/fixtures/longmemeval_memory_grounding_responses.json" \
  --embedding hash --embedding-model hash --dimensions 32 \
  --retrieval-top-k 2 --qa-top-k 1
```

## 恢复运行

```bash
# 恢复所选 arm 最近一次自动命名的运行。
python3 evaluation/longmemeval/run.py --memory-mode raw --resume
python3 evaluation/longmemeval/run.py --memory-mode extracted --resume

# 显式指定的运行目录会原样使用。
python3 evaluation/longmemeval/run.py --resume \
  --run-dir outputs/longmemeval/<你的运行目录>
```

恢复时仍会调用 `memory-bench add --resume`：已有记忆会跳过，未完成的图记录会先补建，
随后再恢复检索。

自动恢复发现按 `--memory-mode` 隔离；raw arm 不会选择 extracted 运行，反之亦然。

## 输出

`outputs/longmemeval/<时间戳>_<模型>_<数据集>_<memory-mode>/`：

```text
config.json                   # source/config/implementation/policy provenance
raw_prepared.json             # 始终保存 source turn prepared 数据
extracted_prepared.json       # 仅 extracted arm 的 Rust 输出
artifacts/                    # Rust 抽取审计 bundle
stages/                       # 可恢复抽取阶段的完成 manifests
store.jsonl                   # 嵌入存储
search_results.json           # 原始检索结果
metrics.json                  # 检索指标
report.html                   # 主 HTML 报告
errors.html                   # QA 错误详情
run_meta.json                 # 运行元数据
qa_results_<tag>.json         # 回答 + 评判标签
qa_metrics_<tag>.json         # QA 准确率、token、延迟
qa_meta_<tag>.json            # QA 配置（用于恢复）
```

raw arm 索引 `raw_prepared.json`，extracted arm 索引 `extracted_prepared.json`。
add、search 和 QA 接收 indexed 路径；retrieval provenance evaluation 始终接收
`raw_prepared.json`，从而由 evidence refs 恢复原始 turn/session ID。

## 测试

```bash
PYTHONPATH=evaluation python -m pytest evaluation/common/metrics_test.py evaluation/longmemeval
```

## 参考文献

- [LongMemEval GitHub](https://github.com/xiaowu0162/longmemeval)
- 论文：*Benchmarking Chat Assistants on Long-Term Interactive Memory* (ICLR 2025)

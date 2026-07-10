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
python3 evaluation/longmemeval/run.py --phase all \
  --embedding openrouter --embedding-model baai/bge-m3 --dimensions 1024 \
  --answerer-model openai/gpt-4o-mini \
  --judge-model openai/gpt-4o-mini \
  --llm-api-key-env OPENROUTER_API_KEY \
  --llm-base-url https://openrouter.ai/api/v1

# 智谱等 OpenAI-compatible 服务只替换 QA 链路；嵌入仍按上面的 embedding 参数配置
export ZHIPU_API_KEY="..."
python3 evaluation/longmemeval/run.py --phase qa --resume \
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
| `--phase` | `retrieval` | `retrieval`、`qa` 或 `all` |
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

# 检索 + QA
python3 evaluation/longmemeval/run.py --phase all \
  --answerer-model openai/gpt-4o-mini \
  --judge-model openai/gpt-4o-mini \
  --llm-api-key-env OPENROUTER_API_KEY \
  --llm-base-url https://openrouter.ai/api/v1
```

## 恢复运行

```bash
python3 evaluation/longmemeval/run.py --resume
python3 evaluation/longmemeval/run.py --resume \
  --run-dir outputs/longmemeval/<你的运行目录>
```

## 输出

`outputs/longmemeval/<时间戳>_<模型>_<数据集>/`：

```
prepared.json                 # 统一数据集
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

## 测试

```bash
PYTHONPATH=evaluation python -m pytest evaluation/common/metrics_test.py evaluation/longmemeval
```

## 参考文献

- [LongMemEval GitHub](https://github.com/xiaowu0162/longmemeval)
- 论文：*Benchmarking Chat Assistants on Long-Term Interactive Memory* (ICLR 2025)

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

## 输出说明

所有评估默认写入仓库根目录下的 `outputs/<数据集>/<时间戳>/`，包含：

- JSON 指标与原始结果
- HTML 报告供快速查看
- `run_meta.json` 含配置与 git hash

大型原始结果不要提交到 Git。用于版本对比的精简记录放在
`evaluation/baselines/`，完整 raw artifact 上传到对象存储、Release 资产或其他制品系统。

## 测试

```bash
PYTHONPATH=evaluation python -m pytest evaluation
cargo test
```

## 参考文献

- PersonaMem: *Know Me, Respond to Me* (NeurIPS 2025)
- LongMemEval: *Benchmarking Chat Assistants on Long-Term Interactive Memory* (ICLR 2025)
- LoCoMo: *Evaluating Very Long-Term Conversational Memory of LLM Agents* (ACL 2024)

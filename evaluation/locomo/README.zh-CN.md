# LoCoMo 评估

**[English](README.md)**

RAM-A 和 `mem0` 的 LoCoMo 评估流水线使用指南。

## 数据集

[LoCoMo](https://github.com/snap-research/locomo)（ACL 2024）评估超长对话记忆能力。
包含 10 段对话，每段约 300 轮、~9K token，跨最多 35 个会话，共 ~1,986 个问题，
覆盖五个类别。

## 环境准备

```bash
pip install -r evaluation/requirements.txt
cargo build

# mem0 后端（可选）：
# pip install "mem0ai>=2.0"

# 默认 smoke fixture 为 evaluation/fixtures/locomo_sample.json
# 完整 benchmark 文件位于 data/locomo/locomo10.json
```

## 环境变量

在 `evaluation/.env` 中设置或直接 export：

| 变量 | 是否必需 | 用途 |
|------|---------|------|
| `OPENAI_API_KEY` | 是 | 回答生成 + 默认 LLM 评判 API key |
| `OPENROUTER_API_KEY` | 是（RAM-A） | 嵌入 API |
| `MODEL` | 否 | 回答模型 + 默认评判模型（默认 `gpt-4o-mini`） |
| `OPENAI_BASE_URL` | 否 | 自定义 API 端点 |
| `JUDGE_MODEL` | 否 | `run_locomo_eval.sh` 的评判模型覆盖项 |
| `LLM_API_KEY_ENV` | 否 | 评判 API key 环境变量覆盖项（默认 `OPENAI_API_KEY`） |
| `LLM_BASE_URL` | 否 | 评判 OpenAI-compatible base URL 覆盖项 |
| `LLM_THINKING` | 否 | 评判供应商 thinking 模式：`default`、`enabled` 或 `disabled` |

常用运行配置也可以通过环境变量覆盖：

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `DATASET` | `fixtures/locomo_sample.json` | 数据集路径（相对于 `evaluation/`） |
| `TOP_K` | `30` | 检索数量 |
| `RUN_ID` | 当前时间戳 | 输出目录名 |
| `RUN_DIR` | `../outputs/locomo/<RUN_ID>` | 自定义输出目录 |

## 模型供应商配置

LoCoMo 的回答生成仍沿用 OpenAI SDK 环境变量。评判阶段已改为 RAM-A 统一 OpenAI-compatible client，并支持和其他 RAM-A 评测脚本一致的供应商参数：

```bash
# OpenRouter
export OPENAI_API_KEY="$OPENROUTER_API_KEY"
export OPENAI_BASE_URL="https://openrouter.ai/api/v1"
export MODEL="openai/gpt-4o-mini"
export LLM_API_KEY_ENV="OPENAI_API_KEY"
export LLM_BASE_URL="$OPENAI_BASE_URL"
export JUDGE_MODEL="$MODEL"

# 智谱等 OpenAI-compatible 服务
export OPENAI_API_KEY="$ZHIPU_API_KEY"
export OPENAI_BASE_URL="https://open.bigmodel.cn/api/coding/paas/v4"
export MODEL="glm-5"
export LLM_API_KEY_ENV="OPENAI_API_KEY"
export LLM_BASE_URL="$OPENAI_BASE_URL"
export JUDGE_MODEL="$MODEL"
```

RAM-A 后端的嵌入阶段由 `memory-bench` 执行，当前仍使用 `OPENROUTER_API_KEY`。也就是说，LoCoMo 可以把回答/评判模型切到其他厂商，但嵌入模型仍按 RAM-A 当前后端配置执行。

## 快速运行

```bash
cd evaluation

# RAM-A 后端
./run_locomo_eval.sh memory_bench

# mem0 后端
./run_locomo_eval.sh mem0
```

Shell 脚本自动运行完整 7 阶段流水线。
输出写入仓库根目录 `outputs/locomo/<RUN_ID>/<backend>/`。

可选的 mem0 对比实现位于 `evaluation/locomo/backends/mem0/`。

## 流水线阶段

```
1. 写入     → 将对话导入记忆存储
2. 检索     → 对每个问题检索记忆
3. 检索指标 → 计算检索命中指标
4. 回答     → 基于检索上下文生成 LLM 回答
5. 评判     → LLM 评判（正确/错误）+ BLEU + F1
6. 指标汇总 → 按类别聚合 QA 指标
7. 报告     → 生成检索 + QA 综合 HTML 报告
```

## 单独运行各脚本

```bash
python3 locomo/locomo_retrieval.py \
  --dataset fixtures/locomo_sample.json --input search_results.json \
  --input-format memory-bench --output-json retrieval_metrics.json \
  --html-report retrieval_report.html

python3 locomo/locomo_responses.py \
  --technique-type memory_bench --dataset fixtures/locomo_sample.json \
  --input search_results.json --output responses.json

python3 locomo/locomo_eval.py \
  --input responses.json --output judge_results.json \
  --judge-model openai/gpt-4o-mini \
  --llm-api-key-env OPENAI_API_KEY \
  --llm-base-url https://openrouter.ai/api/v1

python3 locomo/locomo_metric.py \
  --input judge_results.json --output-json qa_metrics.json \
  --html-report qa_report.html

python3 locomo/write_run_meta.py \
  --output run_meta.json --dataset fixtures/locomo_sample.json \
  --backend RAM-A --phase all --top-k 30 --run-dir .

python3 locomo/locomo_report.py \
  --retrieval-json retrieval_metrics.json --qa-json qa_metrics.json \
  --run-meta run_meta.json --output report.html --errors-output errors.html
```

## 输出

仓库根目录 `outputs/locomo/<时间戳>/`：

```
ram-a/  （或 mem0/）
  store.sqlite                 # SQLite hybrid 存储（仅 RAM-A）
  search_results.json          # 原始检索结果
  retrieval_metrics.json       # 检索命中指标
  responses.json               # LLM 回答
  judge_results.json           # 评判分数 + BLEU + F1
  qa_metrics.json              # 聚合 QA 指标
  report.html                  # 综合 HTML 报告
  errors.html                  # 失败详情
  run_meta.json                # 运行元数据
```

## 注意事项

- 第 5 类（对抗性/不可回答）问题不计入主 QA 分数；如要评估它，需要单独设计拒答/不可回答 rubric
- mem0 检索产出简化报告（缺少轮次路径，无法计算命中指标）
- Shell 脚本会清除上一次输出；使用不同 `RUN_ID` 保留历史结果

## 参考文献

- [LoCoMo GitHub](https://github.com/snap-research/locomo)
- 论文：*Evaluating Very Long-Term Conversational Memory of LLM Agents* (ACL 2024)

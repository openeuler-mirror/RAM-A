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
# 完整 benchmark 是本地下载文件，不提交到仓库：
mkdir -p data/locomo
curl -L https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json \
  -o data/locomo/locomo10.json
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

# 下载 data/locomo/locomo10.json 后运行完整 LoCoMo
DATASET=../data/locomo/locomo10.json ./run_locomo_eval.sh memory_bench

# 图记忆模式：add 阶段构图，search 阶段开启 graph retrieval。
# 需要 OPENROUTER_API_KEY 用于嵌入和图候选抽取。
MEMORY_BENCH_GRAPH=1 \
GRAPH_LLM_MODEL=openai/gpt-4o-mini \
DATASET=../data/locomo/locomo10.json \
./run_locomo_eval.sh memory_bench

# mem0 后端
./run_locomo_eval.sh mem0
```

Shell 脚本自动运行完整 7 阶段流水线。
输出写入仓库根目录 `outputs/locomo/<RUN_ID>/<backend>/`。

可选的 mem0 对比实现位于 `evaluation/locomo/backends/mem0/`。

## 有证据约束的原子记忆 A/B

新的配对实验应使用统一受治理入口。Pilot 前先把下面这份固定 LoCoMo policy 保存为
JSON 文件：

```json
{"schema_version":"locomo-promotion-v1","historical_overall":{"operator":">","threshold":0.4065},"fresh_raw_overall":{"operator":">"},"scored_count":1540,"category_floors":{"1":0.1999,"2":0.4161,"3":0.2717,"4":0.4509},"regression_suite_required":true}
```

```bash
PYTHONPATH=evaluation evaluation/.venv/bin/python evaluation/scripts/run_memory_ab.py \
  --dataset locomo --phase pilot --pair-id locomo-v4 \
  --dataset-file data/locomo/locomo10.json \
  --promotion-policy /absolute/path/locomo-policy.json

PYTHONPATH=evaluation evaluation/.venv/bin/python evaluation/scripts/run_memory_ab.py \
  --dataset locomo --phase full --pair-id locomo-v4 \
  --dataset-file data/locomo/locomo10.json \
  --promotion-policy /absolute/path/locomo-policy.json \
  --frozen-config evaluation/outputs/memory-ab/locomo/pilot/locomo-v4/frozen_config.json
```

Policy 文件字节的 hash 会写入两臂 config 和 frozen manifest。Pilot 不写 history；完整
full pair 即使晋级失败也会记录，但只有通过的 treatment 才能成为 baseline。以上命令
定义 live 协议；人工实际运行前不宣称任何 live 分数或晋级结果。

`run_locomo_memory_ab.sh` 用于评估记忆特性 2 和 4，不改变 answer prompt。配对实验包含两个 arm：

```text
raw：       LoCoMo turn -> prepared-v1 -> 索引原始 turn -> 回答
extracted： LoCoMo turn -> episode/window -> 有证据约束的原子记忆
             -> 只索引原子记忆 -> evidence_refs 展开为精确原文 -> 回答
```

Treatment 不会把原始 turn 和原子记忆共同写入索引。原始 turn 只保存在
`raw_prepared.json` 中，供命中的 atomic claim 展开 speaker、timestamp、quote
和完整 source text。

请使用新轮换的 OpenRouter key，不要把真实 key 写入命令、README、artifact 或 commit：

```bash
cd evaluation
export OPENROUTER_API_KEY="<new-rotated-key>"

PYTHON_BIN=../.venv/bin/python \
PHASE=pilot RUN_DIR=outputs/locomo-memory-ab/pilot \
./run_locomo_memory_ab.sh

PYTHON_BIN=../.venv/bin/python \
PHASE=full \
FROZEN_CONFIG=outputs/locomo-memory-ab/pilot/frozen_config.json \
RUN_DIR=outputs/locomo-memory-ab/full \
./run_locomo_memory_ab.sh
```

Pilot 固定使用 conversation index 0。Pilot 通过后会把 model、window、retrieval
和 rerank 配置冻结到 `frozen_config.json`；full run 会在任何模型调用前拒绝配置
不一致。固定模型为：extraction、grounding、answer、judge 均使用
`openai/gpt-4o-mini`；embedding 使用 1,024 维 `baai/bge-m3`；rerank 使用
`cohere/rerank-v3.5`。Hybrid 权重为 0.7/0.3，candidate K 为 150，rerank input K
为 40，最终 Top K 为 30。

每个 arm 都保存 `config.json`、prepared input、SQLite store、search result、
retrieval diagnostics、response、judge result、QA metric、HTML report、逐 query
版本化 cache，以及 `stages/*.complete.json`。Treatment 还在 `artifacts/` 中保存
normalized message、episode、window、accepted/rejected/quarantined memory 和 extraction
健康指标。只有 source、configuration、command 和 output hash 全部匹配才允许 resume。

历史 v3 overall 门槛为 0.4065；category 1–4 的下限依次为 0.1999、0.4161、
0.2717、0.4509。可晋级的 full treatment 必须同时严格超过 0.4065 和 fresh paired
raw，包含恰好 1,540 个计分问题，满足全部 category 下限，并通过完整 Python、Rust、
shell 和离线 smoke 回归。如果任一检查失败，接入代码必须保持未提交，通过
`comparison.html` 和 pipeline artifact 诊断，也不能把该结果登记为新 baseline。

图记忆相关环境变量：

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `MEMORY_BENCH_GRAPH` | `0` | 设为 `1` 后给 RAM-A add/search 传入 graph 参数 |
| `GRAPH_WEIGHT` | `0.2` | graph retrieval 融合权重 |
| `GRAPH_FAIL_OPEN` | `0` | 设为 `1` 后 graph search 失败时退化为非 graph 检索 |
| `GRAPH_MEMORY_SPACE_MODE` | `auto` | `memory-bench` 推导 memory space 的方式 |
| `GRAPH_MEMORY_SPACE_FIELD` | `scope_id` | `metadata-field` 模式使用的 metadata/filter 字段 |
| `GRAPH_OWNER_ID` | `benchmark` | graph memory owner id |
| `GRAPH_LLM_API_KEY_ENV` | `OPENROUTER_API_KEY` | 图候选抽取 API key 所在环境变量 |
| `GRAPH_LLM_MODEL` | `openai/gpt-4o-mini` | 图候选抽取模型 |
| `GRAPH_LLM_BASE_URL` | `https://openrouter.ai/api/v1` | OpenAI-compatible 图候选抽取 base URL |
| `GRAPH_LLM_TIMEOUT_MS` | `60000` | 图候选抽取超时 |

当 `MEMORY_BENCH_GRAPH=1` 时，shell wrapper 会给 `memory-bench add` 传入
`--graph-build`，给 `memory-bench search` 传入 `--graph`。`GRAPH_LLM_MODEL`
会映射为 `memory-bench` 的 `--graph-llm-model`。

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
- 原子记忆 runner 不会清理输出；它只 resume hash 完全匹配的 stage

## 参考文献

- [LoCoMo GitHub](https://github.com/snap-research/locomo)
- [LoCoMo locomo10.json](https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json)
- 论文：*Evaluating Very Long-Term Conversational Memory of LLM Agents* (ACL 2024)

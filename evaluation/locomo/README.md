# LoCoMo Evaluation

**[中文](README.zh-CN.md)**

Usage guide for the LoCoMo evaluation pipeline for RAM-A and `mem0`.

## Dataset

[LoCoMo](https://github.com/snap-research/locomo) (ACL 2024) evaluates very long-term
conversational memory. 10 conversations, ~300 turns and ~9K tokens each across up to 35
sessions, ~1,986 questions in five categories.

## Setup

```bash
pip install -r evaluation/requirements.txt
cargo build

# mem0 backend (optional):
# pip install "mem0ai>=2.0"

# The default smoke fixture is evaluation/fixtures/locomo_sample.json
# The full benchmark is a local download, not a checked-in file:
mkdir -p data/locomo
curl -L https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json \
  -o data/locomo/locomo10.json
```

## Environment Variables

Set in `evaluation/.env` or export:

| Variable | Required | Purpose |
|----------|----------|---------|
| `OPENAI_API_KEY` | Yes | Answer generation + default LLM judge API key |
| `OPENROUTER_API_KEY` | Yes (RAM-A) | Embedding API |
| `MODEL` | No | Answer model + default judge model (default: `gpt-4o-mini`) |
| `OPENAI_BASE_URL` | No | Custom API endpoint |
| `JUDGE_MODEL` | No | Judge model override for `run_locomo_eval.sh` |
| `LLM_API_KEY_ENV` | No | Judge API key env var override (default: `OPENAI_API_KEY`) |
| `LLM_BASE_URL` | No | Judge OpenAI-compatible base URL override |
| `LLM_THINKING` | No | Judge provider thinking mode: `default`, `enabled`, or `disabled` |

Common run settings can also be overridden with environment variables:

| Variable | Default | Purpose |
|----------|---------|---------|
| `DATASET` | `fixtures/locomo_sample.json` | Dataset path relative to `evaluation/` |
| `TOP_K` | `30` | Retrieval count |
| `RUN_ID` | current timestamp | Output directory name |
| `RUN_DIR` | `../outputs/locomo/<RUN_ID>` | Custom output directory |

## Model Provider Configuration

LoCoMo answer generation still follows the OpenAI SDK environment variables. The judge stage uses the shared RAM-A OpenAI-compatible client and accepts the same provider knobs as other RAM-A evaluation scripts:

```bash
# OpenRouter
export OPENAI_API_KEY="$OPENROUTER_API_KEY"
export OPENAI_BASE_URL="https://openrouter.ai/api/v1"
export MODEL="openai/gpt-4o-mini"
export LLM_API_KEY_ENV="OPENAI_API_KEY"
export LLM_BASE_URL="$OPENAI_BASE_URL"
export JUDGE_MODEL="$MODEL"

# Zhipu or another OpenAI-compatible service
export OPENAI_API_KEY="$ZHIPU_API_KEY"
export OPENAI_BASE_URL="https://open.bigmodel.cn/api/coding/paas/v4"
export MODEL="glm-5"
export LLM_API_KEY_ENV="OPENAI_API_KEY"
export LLM_BASE_URL="$OPENAI_BASE_URL"
export JUDGE_MODEL="$MODEL"
```

The RAM-A embedding stage is executed by `memory-bench` and currently still uses `OPENROUTER_API_KEY`. In other words, LoCoMo can switch answer/judge providers through the variables above, while embeddings follow the current RAM-A backend configuration.

## Quick Run

```bash
cd evaluation

# RAM-A backend
./run_locomo_eval.sh memory_bench

# Full LoCoMo benchmark after downloading data/locomo/locomo10.json
DATASET=../data/locomo/locomo10.json ./run_locomo_eval.sh memory_bench

# mem0 backend
./run_locomo_eval.sh mem0
```

The shell script runs the full 7-stage pipeline.
Outputs are written to repository-root `outputs/locomo/<RUN_ID>/<backend>/`.

The optional mem0 comparison implementation lives under `evaluation/locomo/backends/mem0/`.

## Pipeline Stages

```
1. Add       → ingest conversations into memory store
2. Search    → retrieve memories for each question
3. Retrieval → compute evidence-hit metrics
4. Answer    → generate LLM answers from retrieved context
5. Judge     → LLM judge (CORRECT/WRONG) + BLEU + F1
6. Metrics   → aggregate QA metrics by category
7. Report    → combined retrieval + QA HTML report
```

## Individual Scripts

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

## Output

Repository-root `outputs/locomo/<timestamp>/`:

```
ram-a/  (or mem0/)
  store.sqlite                 # SQLite hybrid store (RAM-A only)
  search_results.json          # raw retrieval results
  retrieval_metrics.json       # evidence hit metrics
  responses.json               # LLM answers
  judge_results.json           # judge scores + BLEU + F1
  qa_metrics.json              # aggregated QA metrics
  report.html                  # combined HTML report
  errors.html                  # failure details
  run_meta.json                # run metadata
```

## Notes

- Category 5 (adversarial/unanswerable) questions are excluded from the main QA score; evaluate them separately with an abstention/unanswerable rubric if needed
- mem0 retrieval produces a stub report (no turn-path evidence)
- Shell script cleans previous outputs; use different `RUN_ID` to preserve

## Reference

- [LoCoMo GitHub](https://github.com/snap-research/locomo)
- [LoCoMo locomo10.json](https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json)
- Paper: *Evaluating Very Long-Term Conversational Memory of LLM Agents* (ACL 2024)

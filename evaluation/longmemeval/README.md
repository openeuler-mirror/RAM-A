# LongMemEval Evaluation

**[中文](README.zh-CN.md)**

LongMemEval evaluation runner for RAM-A.

## Dataset

[LongMemEval](https://github.com/xiaowu0162/longmemeval) (ICLR 2025) contains 500 questions
testing five core long-term memory abilities: information extraction, multi-session reasoning,
knowledge updates, temporal reasoning, and preference recall. This runner uses
`longmemeval_oracle.json`, meaning each question is searched within its oracle
conversation scope.

## Setup

```bash
pip install -r evaluation/requirements.txt
cargo build

mkdir -p data/longmemeval
wget https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned/resolve/main/longmemeval_oracle.json \
  -O data/longmemeval/longmemeval_oracle.json
```

## Environment Variables

| Variable | Required |
|----------|----------|
| `OPENROUTER_API_KEY` | Yes for real runs (embeddings + LLM) |

For other OpenAI-compatible providers, set `--api-key-env`, `--llm-api-key-env`,
and `--llm-base-url`.

## Model Provider Configuration

LongMemEval has two model paths:

- Embedding path: `--embedding openrouter` calls OpenRouter embeddings. Configure `--embedding-model`, `--dimensions`, and `--api-key-env`. The current CLI does not expose an embedding base URL, so direct non-OpenRouter embedding providers require extending `memory-bench`.
- QA path: answerer and judge use OpenAI-compatible chat completions. Configure provider with `--answerer-model`, `--judge-model`, `--llm-api-key-env`, and `--llm-base-url`.

Examples:

```bash
# OpenRouter (default)
export OPENROUTER_API_KEY="..."
python3 evaluation/longmemeval/run.py --phase all \
  --embedding openrouter --embedding-model baai/bge-m3 --dimensions 1024 \
  --answerer-model openai/gpt-4o-mini \
  --judge-model openai/gpt-4o-mini \
  --llm-api-key-env OPENROUTER_API_KEY \
  --llm-base-url https://openrouter.ai/api/v1

# Zhipu or another OpenAI-compatible service for QA only;
# embeddings are still controlled by the embedding options above.
export ZHIPU_API_KEY="..."
python3 evaluation/longmemeval/run.py --phase qa --resume \
  --run-dir outputs/longmemeval/<your-run-dir> \
  --answerer-model glm-5 \
  --judge-model glm-5 \
  --llm-api-key-env ZHIPU_API_KEY \
  --llm-base-url https://open.bigmodel.cn/api/coding/paas/v4 \
  --llm-thinking disabled
```

`--llm-thinking` is provider-specific and mainly useful for GLM-style models that may return reasoning content. Keep it as `default` for regular OpenRouter/OpenAI models.

## Commands

```bash
python3 evaluation/longmemeval/run.py [options]
```

### Key Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `--phase` | `retrieval` | `retrieval`, `qa`, or `all` |
| `--backend` | `RAM-A` | RAM-A backend key |
| `--embedding` | `openrouter` | `openrouter` or `hash` |
| `--embedding-model` | `baai/bge-m3` | Embedding model |
| `--dimensions` | 1024 | Embedding dimensions |
| `--api-key-env` | `OPENROUTER_API_KEY` | Env var for embedding API key |
| `--retrieval-top-k` | 10 | Top-k for search |
| `--embedding-batch-size` | 64 | Batch size for embedding calls |
| `--resume` | false | Skip completed steps |
| `--run-dir` | *(auto)* | Output directory |
| `--max-questions` | *(all)* | Limit for smoke tests |
| **QA stage** | | |
| `--answerer-model` | `openai/gpt-4o-mini` | Answer generation model |
| `--judge-model` | `openai/gpt-4o-mini` | LLM-as-judge model |
| `--llm-api-key-env` | `OPENROUTER_API_KEY` | Env var for LLM API key |
| `--llm-base-url` | `https://openrouter.ai/api/v1` | OpenAI-compatible base URL |
| `--qa-top-k` | 10 | Memories used for QA |
| `--answer-prompt-version` | `lme_default` | Prompt template version |
| `--memory-format` | `full` | `full` or `compact` |
| `--show-scores` | false | Include retrieval scores in prompt |
| `--qa-output-tag` | *(auto)* | Override QA filename tag |
| `--llm-thinking` | `default` | `default`/`enabled`/`disabled` |

Full list: `python3 evaluation/longmemeval/run.py --help`

## Quick Smoke Test

```bash
python3 evaluation/longmemeval/run.py \
  --embedding hash --embedding-model hash --dimensions 128 --max-questions 5
```

## Full Run

```bash
export OPENROUTER_API_KEY="your-key"

# Retrieval only
python3 evaluation/longmemeval/run.py

# Retrieval + QA
python3 evaluation/longmemeval/run.py --phase all \
  --answerer-model openai/gpt-4o-mini \
  --judge-model openai/gpt-4o-mini \
  --llm-api-key-env OPENROUTER_API_KEY \
  --llm-base-url https://openrouter.ai/api/v1
```

## Resume

```bash
python3 evaluation/longmemeval/run.py --resume
python3 evaluation/longmemeval/run.py --resume \
  --run-dir outputs/longmemeval/<your-run-dir>
```

## Output

`outputs/longmemeval/<timestamp>_<model-slug>_<dataset>/`:

```
prepared.json                 # unified dataset
store.jsonl                   # embedding store
search_results.json           # raw retrieval results
metrics.json                  # retrieval metrics
report.html                   # main HTML report
errors.html                   # QA error details
run_meta.json                 # run metadata
qa_results_<tag>.json         # answers + judge labels
qa_metrics_<tag>.json         # QA accuracy, tokens, latency
qa_meta_<tag>.json            # QA config for resume
```

## Tests

```bash
PYTHONPATH=evaluation python -m pytest evaluation/common/metrics_test.py evaluation/longmemeval
```

## Reference

- [LongMemEval GitHub](https://github.com/xiaowu0162/longmemeval)
- Paper: *Benchmarking Chat Assistants on Long-Term Interactive Memory* (ICLR 2025)

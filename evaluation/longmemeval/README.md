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
python3 evaluation/longmemeval/run.py --pipeline-phase all \
  --embedding openrouter --embedding-model baai/bge-m3 --dimensions 1024 \
  --answerer-model openai/gpt-4o-mini \
  --judge-model openai/gpt-4o-mini \
  --llm-api-key-env OPENROUTER_API_KEY \
  --llm-base-url https://openrouter.ai/api/v1

# Zhipu or another OpenAI-compatible service for QA only;
# embeddings are still controlled by the embedding options above.
export ZHIPU_API_KEY="..."
python3 evaluation/longmemeval/run.py --pipeline-phase qa --resume \
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
| `--memory-mode` | `raw` | Index `raw` turns or Rust-produced `extracted` memories |
| `--phase` | `pilot` | Experiment governance phase: `pilot` or `full` |
| `--pipeline-phase` | `retrieval` | Execute `retrieval`, `qa`, or `all` stages |
| `--pair-id` | `standalone` | Stable identifier shared by paired arms |
| `--frozen-config` | *(none)* | Frozen immutable manifest; required for `full` |
| `--promotion-policy` | *(none)* | Promotion policy hashed into the manifest; required for `full` |
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
| **Extraction stage** | | |
| `--extraction-model` | `openai/gpt-4o-mini` | Atomic-memory extraction model |
| `--verifier-model` | `openai/gpt-4o-mini` | Grounding verifier model |
| `--extraction-cache-dir` | `<run>/cache/memory-pipeline` | Extraction cache directory |
| `--max-candidate-tokens` | 320 | Candidate window budget |
| `--max-window-tokens` | 640 | Candidate plus context budget |
| `--context-before-messages` | 2 | Previous messages included as context |
| `--context-after-messages` | 0 | Following messages included as context |
| `--extractor-responses` / `--grounding-responses` | *(none)* | Paired response maps for fully offline extraction |
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

For compatibility, legacy `--phase retrieval|qa|all` (including
`--phase=value`) is rewritten to `--pipeline-phase` with a deprecation warning.
Combining that legacy spelling with an explicit `--pipeline-phase` is an error.

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

# Retrieval + QA pilot arm
python3 evaluation/longmemeval/run.py --phase pilot --pipeline-phase all \
  --answerer-model openai/gpt-4o-mini \
  --judge-model openai/gpt-4o-mini \
  --llm-api-key-env OPENROUTER_API_KEY \
  --llm-base-url https://openrouter.ai/api/v1
```

A governed `--phase full` run additionally requires both `--frozen-config` and
`--promotion-policy`. Their immutable fields and policy hash are validated
before preprocessing or constructing any embedding/chat client.

## Fully Offline Extracted Fixture

This smoke run uses the checked-in paired response maps and hash embeddings. It
does not require an API key and does not produce an official benchmark score.

```bash
python3 evaluation/longmemeval/run.py \
  --dataset-file "$PWD/evaluation/fixtures/longmemeval_sample.json" \
  --run-dir /tmp/longmemeval-extracted-offline \
  --memory-mode extracted --phase pilot --pipeline-phase retrieval \
  --pair-id offline-longmemeval \
  --extractor-responses "$PWD/evaluation/fixtures/longmemeval_memory_extractor_responses.json" \
  --grounding-responses "$PWD/evaluation/fixtures/longmemeval_memory_grounding_responses.json" \
  --embedding hash --embedding-model hash --dimensions 32 \
  --retrieval-top-k 2 --qa-top-k 1
```

## Resume

```bash
# Resume the latest automatic run for the selected arm.
python3 evaluation/longmemeval/run.py --memory-mode raw --resume
python3 evaluation/longmemeval/run.py --memory-mode extracted --resume

# An explicit run directory is used unchanged.
python3 evaluation/longmemeval/run.py --resume \
  --run-dir outputs/longmemeval/<your-run-dir>
```

Automatic resume discovery is scoped to `--memory-mode`; a raw arm never
selects an extracted run, and vice versa.

## Output

`outputs/longmemeval/<timestamp>_<model-slug>_<dataset>_<memory-mode>/`:

```text
config.json                   # source/config/implementation/policy provenance
raw_prepared.json             # always the source-turn prepared dataset
extracted_prepared.json       # Rust output for an extracted arm only
artifacts/                    # Rust extraction audit bundle
stages/                       # resumable extraction completion manifests
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

Raw arms index `raw_prepared.json`; extracted arms index
`extracted_prepared.json`. Add, search, and QA receive that indexed path, while
retrieval provenance evaluation always receives `raw_prepared.json` so evidence
references can recover original source turn and session IDs.

## Tests

```bash
PYTHONPATH=evaluation python -m pytest evaluation/common/metrics_test.py evaluation/longmemeval
```

## Reference

- [LongMemEval GitHub](https://github.com/xiaowu0162/longmemeval)
- Paper: *Benchmarking Chat Assistants on Long-Term Interactive Memory* (ICLR 2025)

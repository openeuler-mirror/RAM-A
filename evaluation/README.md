# Evaluation

**[中文](README.zh-CN.md)**

Entry point for benchmarking the RAM-A memory system. This file is a usage guide; dataset-specific parameters and staged commands live in each dataset README.

## Datasets

| Dataset | Focus | Questions | Source / download | Local placement |
|---------|-------|-----------|-------------------|-----------------|
| PersonaMem | Personalized multiple-choice answering from long user profiles | 589 questions for 32k; additional 128k and 1M splits | [GitHub](https://github.com/bowen-upenn/PersonaMem), [HuggingFace](https://huggingface.co/datasets/bowen-upenn/PersonaMem) | `data/personalmem/raw/`, then `data/personalmem/prepared/` |
| LongMemEval | Long-term memory QA across multi-session chats | 500 | [GitHub](https://github.com/xiaowu0162/longmemeval), [cleaned oracle JSON](https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned/resolve/main/longmemeval_oracle.json) | `data/longmemeval/longmemeval_oracle.json` |
| LoCoMo | Very long-term conversational memory QA | ~1,986 questions | [GitHub](https://github.com/snap-research/locomo), [locomo10.json](https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json) | `data/locomo/locomo10.json` |

Full benchmark datasets are local downloads only. The repository keeps small fixtures
under `evaluation/fixtures/` for smoke tests.

## Setup

```bash
python3 -m venv evaluation/.venv
source evaluation/.venv/bin/activate
pip install -r evaluation/requirements.txt
cargo build

export OPENROUTER_API_KEY="..."   # LongMemEval / PersonaMem default embeddings and answers
export OPENAI_API_KEY="..."       # LoCoMo answers and judge; can be paired with OPENAI_BASE_URL
```

Run commands from the repository root unless noted. The LoCoMo shell script is run from `evaluation/`.
To run LoCoMo with OpenRouter, set `OPENAI_API_KEY="$OPENROUTER_API_KEY"` and
`OPENAI_BASE_URL=https://openrouter.ai/api/v1`.

## Other Model Providers

The benchmark uses two different model types, and they are configured differently:

| Stage | Purpose | Current configuration |
|-------|---------|-----------------------|
| Embedding | Vectorize memories and queries for add/search | `memory-bench` currently supports `--embedding openrouter` or `--embedding hash`; configure `--model`, `--dimensions`, and `--api-key-env` |
| Answer / judge | Generate answers and run LLM-as-judge scoring | OpenAI-compatible chat completions; configure model name, API-key env var, and base URL |

`hash` is for smoke tests only and is not a real semantic embedding baseline. The current CLI does not expose a custom embedding base URL; direct non-OpenRouter embedding providers require extending `memory-bench`, or routing through OpenRouter.

OpenAI-compatible answer/judge providers can be configured like this:

```bash
# OpenRouter
export OPENROUTER_API_KEY="..."
# LongMemEval: --llm-api-key-env OPENROUTER_API_KEY --llm-base-url https://openrouter.ai/api/v1
# PersonaMem:  --answer-api-key-env OPENROUTER_API_KEY --answer-base-url https://openrouter.ai/api/v1

# Example: Zhipu or another OpenAI-compatible endpoint
export ZHIPU_API_KEY="..."
# LongMemEval: --answerer-model glm-5 --judge-model glm-5 \
#   --llm-api-key-env ZHIPU_API_KEY \
#   --llm-base-url https://open.bigmodel.cn/api/coding/paas/v4
# PersonaMem: --answer-model glm-5 \
#   --answer-api-key-env ZHIPU_API_KEY \
#   --answer-base-url https://open.bigmodel.cn/api/coding/paas/v4

# LoCoMo answer generation uses OpenAI SDK env vars; judge uses the shared LLM knobs
export OPENAI_API_KEY="$ZHIPU_API_KEY"
export OPENAI_BASE_URL="https://open.bigmodel.cn/api/coding/paas/v4"
export MODEL="glm-5"
export LLM_API_KEY_ENV="OPENAI_API_KEY"
export LLM_BASE_URL="$OPENAI_BASE_URL"
export JUDGE_MODEL="$MODEL"
```

For LoCoMo, the RAM-A embedding stage still needs `OPENROUTER_API_KEY` unless another embedding backend is added.

## Quick Start

```bash
# PersonaMem (32k, smoke test)
python evaluation/personalmem/run.py official-pipeline \
  --size 32k --limit-questions 5 --embedding hash

# LongMemEval (smoke test)
python3 evaluation/longmemeval/run.py \
  --embedding hash --embedding-model hash --dimensions 128 --max-questions 5

# LoCoMo (full pipeline)
cd evaluation && ./run_locomo_eval.sh memory_bench
```

## Grounded Memory Preparation (Features 2 and 4)

Dataset adapters first produce raw `benchmark-prepared-v1` conversation records. The
memory pipeline then groups messages into episodes, creates candidate-owned extraction
windows with optional overlapping context, extracts atomic memories, validates exact
source quotes, and promotes only memories whose grounding result is `SUPPORTED`.

Create a raw prepared file with an existing adapter. For example:

```bash
# PersonaMem (after downloading the official files)
python evaluation/personalmem/run.py prepare \
  --size 32k \
  --schema-version benchmark-prepared-v1 \
  --prepared-dataset outputs/personalmem/raw-prepared.json

# LongMemEval
PYTHONPATH=evaluation python -c \
  'from longmemeval.preprocess import preprocess; preprocess("data/longmemeval/longmemeval_oracle.json", "outputs/longmemeval/raw-prepared.json")'
```

Run extraction and independent grounding verification with any OpenAI-compatible chat
endpoint:

```bash
PYTHONPATH=evaluation python -m common.memory_pipeline.cli \
  --input outputs/longmemeval/raw-prepared.json \
  --output outputs/longmemeval/extracted-prepared.json \
  --artifacts-dir outputs/longmemeval/memory-pipeline \
  --model openai/gpt-4o-mini \
  --verifier-model openai/gpt-4o-mini \
  --api-key-env OPENROUTER_API_KEY \
  --cache-dir outputs/longmemeval/memory-pipeline-cache
```

Use `extracted-prepared.json` as the input to the existing add/search path. Its records
use `metadata.memory_kind=extracted_memory`; no Rust memory schema or search behavior
changes are required. The artifact directory contains normalized messages, episodes,
extraction windows, raw candidates, accepted memories, rejected/quarantined records,
token/cache statistics, and deterministic run metadata. Episodes and windows are audit
and extraction units, not records to embed or index. Only the accepted memories in the
output prepared file are indexed.

Offline fixture mode replaces both model calls with JSON response maps:

```bash
PYTHONPATH=evaluation python -m common.memory_pipeline.cli \
  --input outputs/longmemeval/raw-prepared.json \
  --output /tmp/extracted-prepared.json \
  --artifacts-dir /tmp/memory-pipeline \
  --extractor-responses /tmp/extraction-responses.json \
  --grounding-responses /tmp/grounding-responses.json
```

The extraction map is keyed by deterministic window ID and each value is an
`atomic_memory_v1` response object. The grounding map is keyed by deterministic
candidate-memory ID and each value is a grounding status or `{status, reason}` object.
Both files are required: the CLI never promotes unverified memories. Live mode makes
one extraction call per uncached window and one verification call per window containing
valid candidates; inspect `extraction_stats.json` before estimating or comparing cost.

## Output

All evaluation runs write to repository-root `outputs/<dataset>/<timestamp>/` by default, containing:

- JSON metrics and raw results
- HTML reports for quick inspection
- `run_meta.json` with configuration and git hash

Keep large raw artifacts out of Git. Store compact comparison records under
`evaluation/baselines/` and upload full run artifacts to object storage, release assets,
or another artifact system.

## Tests

```bash
PYTHONPATH=evaluation python -m pytest evaluation
cargo test
```

## References

- PersonaMem: *Know Me, Respond to Me* (NeurIPS 2025)
- LongMemEval: *Benchmarking Chat Assistants on Long-Term Interactive Memory* (ICLR 2025)
- LoCoMo: *Evaluating Very Long-Term Conversational Memory of LLM Agents* (ACL 2024)

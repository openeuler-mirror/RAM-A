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

# Graph memory mode. This builds graph memory during add and enables graph retrieval
# during search. It needs OPENROUTER_API_KEY for embeddings and graph extraction.
MEMORY_BENCH_GRAPH=1 \
GRAPH_LLM_MODEL=openai/gpt-4o-mini \
DATASET=../data/locomo/locomo10.json \
./run_locomo_eval.sh memory_bench

# mem0 backend
./run_locomo_eval.sh mem0
```

The shell script runs the full 7-stage pipeline.
Outputs are written to repository-root `outputs/locomo/<RUN_ID>/<backend>/`.
For the RAM-A backend, the wrapper first converts the raw LoCoMo file into
`outputs/locomo/<RUN_ID>/ram-a/prepared.json` using the unified
`benchmark-prepared-v1` schema. The prepared memories preserve LoCoMo-specific
fields such as `raw_memory_path`, `session_timestamp`, and `observed_at_ms` in
metadata so graph extraction can use observation time without adding LoCoMo
parsing logic to `memory-bench`. When a turn has a speaker, the adapter also maps it to the
generic `metadata.graph_source_entity` contract, so graph provenance can link the source record
to its author without treating the author link as a fact.

The optional mem0 comparison implementation lives under `evaluation/locomo/backends/mem0/`.

## Grounded Atomic-Memory A/B

The governed entrypoint supersedes the shell wrapper for new paired runs. Save
this exact frozen LoCoMo policy before the pilot:

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

The policy bytes are hashed into both arm configs and the frozen manifest.
Pilot produces no history. A complete full pair is recorded even if promotion
fails, while only a passing treatment can become a baseline. The commands above
describe the live protocol; they do not claim a live result until run manually.

`run_locomo_memory_ab.sh` evaluates memory features 2 and 4 without changing the
answer prompt. The paired arms are:

```text
raw:       LoCoMo turns -> prepared-v1 -> index raw turns -> answer
extracted: LoCoMo turns -> episode/window -> grounded atomic memories
           -> index atomic memories -> expand evidence_refs to exact source turns -> answer
```

Raw source turns are never co-indexed with atomic memories in the treatment arm.
They remain in `raw_prepared.json` only so a retrieved atomic claim can be rendered
with its original speaker, timestamp, quote, and full source text.

Use a newly rotated OpenRouter key and never put a real key in a command, README,
artifact, or commit:

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

Pilot is fixed to conversation index 0. A passing pilot freezes the model,
window, retrieval, and rerank configuration in `frozen_config.json`; full runs
reject any mismatch before a model call. The fixed models are
`openai/gpt-4o-mini` for extraction, grounding, answer, and judge;
`baai/bge-m3` (1,024 dimensions) for embeddings; and
`cohere/rerank-v3.5` for reranking. Hybrid weights are 0.7/0.3, candidate K is
150, rerank input K is 40, and final Top K is 30.

Each arm contains `config.json`, prepared input, SQLite store, search results,
retrieval diagnostics, responses, judge results, QA metrics, HTML reports,
versioned per-query caches, and `stages/*.complete.json`. The treatment also
contains normalized messages, episodes, windows, accepted/rejected/quarantined
memories, and extraction health statistics under `artifacts/`. Resume occurs
only when the source, configuration, command, and output hashes still match.

The historical v3 gate is 0.4065 overall, with category floors 0.1999,
0.4161, 0.2717, and 0.4509 for categories 1–4. A promotable full treatment must
score strictly above both 0.4065 and its fresh paired raw arm, contain exactly
1,540 scored questions, meet every category floor, and pass the complete Python,
Rust, shell, and offline smoke regression suite. If any check fails, keep the
integration code uncommitted and use `comparison.html` plus pipeline artifacts
for diagnosis; do not record the run as a new baseline.

Graph-specific environment variables:

| Variable | Default | Purpose |
|----------|---------|---------|
| `MEMORY_BENCH_GRAPH` | `0` | Set to `1` to pass graph flags to RAM-A add/search |
| `GRAPH_WEIGHT` | `0.2` | Graph retrieval fusion weight |
| `GRAPH_RERANK` | `0` | Set to `1` to apply weighted reciprocal rank fusion |
| `GRAPH_ALLOW_GRAPH_ONLY` | `0` | Set to `1` to admit graph-only evidence records |
| `GRAPH_MAX_GRAPH_ONLY_RESULTS` | *(20% of top-k)* | Maximum graph-only records in the final result |
| `GRAPH_FAIL_OPEN` | `0` | Set to `1` to fall back to non-graph retrieval if graph search fails |
| `GRAPH_MEMORY_SPACE_MODE` | `auto` | Memory-space derivation mode for `memory-bench` |
| `GRAPH_MEMORY_SPACE_FIELD` | `scope_id` | Metadata/filter field used when mode is `metadata-field` |
| `GRAPH_OWNER_ID` | `benchmark` | Graph memory owner id |
| `GRAPH_LLM_API_KEY_ENV` | `OPENROUTER_API_KEY` | Env var containing graph extraction API key |
| `GRAPH_LLM_MODEL` | `openai/gpt-4o-mini` | Graph extraction model |
| `GRAPH_LLM_BASE_URL` | `https://openrouter.ai/api/v1` | OpenAI-compatible graph extraction base URL |
| `GRAPH_LLM_TIMEOUT_MS` | `60000` | Graph extraction timeout |
| `GRAPH_BUILD_CONCURRENCY` | `1` | Maximum concurrent graph-build records; increase gradually if the provider permits |

When `MEMORY_BENCH_GRAPH=1`, the shell wrapper passes `--graph-build` to
`memory-bench add` and `--graph` to `memory-bench search`. `GRAPH_LLM_MODEL`
maps to the `memory-bench` `--graph-llm-model` flag.
`GRAPH_BUILD_CONCURRENCY` maps only to add-stage `--graph-build-concurrency`; the default `1`
preserves serial build behavior.
In the default `auto` memory-space mode, prepared LoCoMo records use
`scope_id` values such as `path:$[0]`. If a graph build is resumed, completed
graph runs are skipped, missing graph runs are built, and failed/running graph
runs fail explicitly instead of silently producing a partial graph benchmark.

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

python3 graph_audit.py \
  --store ../outputs/locomo/<RUN_ID>/ram-a/store.sqlite \
  --output ../outputs/locomo/<RUN_ID>/ram-a/graph_audit.json

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
- The atomic-memory runner does not clean outputs; it resumes only hash-matching stages

## Reference

- [LoCoMo GitHub](https://github.com/snap-research/locomo)
- [LoCoMo locomo10.json](https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json)
- Paper: *Evaluating Very Long-Term Conversational Memory of LLM Agents* (ACL 2024)

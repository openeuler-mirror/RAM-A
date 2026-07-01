# LoCoMo Evaluation Guide

This guide describes how to run the LoCoMo pipeline and how to interpret the generated
artifacts.

## Entry Point

Run from `evaluation/`:

```bash
cd evaluation
./run_locomo_eval.sh memory_bench
```

The mem0 comparison backend is optional and requires `mem0ai`:

```bash
cd evaluation
./run_locomo_eval.sh mem0
```

The default dataset fixture is the tiny synthetic
`evaluation/fixtures/locomo_sample.json` and the default retrieval count is `TOP_K=30`.
For the full LoCoMo benchmark, use `DATASET=../data/locomo/locomo10.json`.

```bash
cd evaluation
TOP_K=20 DATASET=fixtures/locomo_sample.json ./run_locomo_eval.sh memory_bench
TOP_K=30 DATASET=../data/locomo/locomo10.json ./run_locomo_eval.sh memory_bench
```

## Environment

The pipeline reads `evaluation/.env` and the current environment:

```.env
OPENAI_API_KEY="sk-..."
OPENAI_BASE_URL="https://openrouter.ai/api/v1" # optional OpenAI-compatible endpoint
MODEL="openai/gpt-4o-mini"

OPENROUTER_API_KEY="sk-..." # RAM-A embedding stage
MEM0_TELEMETRY=False
```

`OPENAI_API_KEY` is used by answer generation and is the default API key env for the
LLM judge. `OPENROUTER_API_KEY` is used by the RAM-A embedding stage unless another
embedding backend is added.

The judge stage now uses the shared RAM-A OpenAI-compatible client. Override it with:

```bash
JUDGE_MODEL="openai/gpt-4o-mini"
LLM_API_KEY_ENV="OPENAI_API_KEY"
LLM_BASE_URL="https://openrouter.ai/api/v1"
LLM_THINKING="default"
```

## Pipeline

`./run_locomo_eval.sh memory_bench` runs:

1. `memory-bench add`: ingest LoCoMo conversation text into a SQLite hybrid store.
2. `memory-bench search`: retrieve top-k memories for each question.
3. `locomo_retrieval.py`: compute evidence-hit retrieval metrics.
4. `locomo_responses.py`: generate answers from retrieved context.
5. `locomo_eval.py`: score answers with BLEU, F1, and LLM judge. The scoring rubric and
   `judge_results.json` schema are unchanged; only the LLM call path and JSON label parser
   are unified with the RAM-A evaluation client.
6. `locomo_metric.py`: aggregate QA metrics.
7. `locomo_report.py`: render the combined HTML report.

`./run_locomo_eval.sh mem0` uses `locomo_experiments.py` for add/search, then runs the
same answer, judge, metric, and report stages.
The optional mem0 comparison implementation is LoCoMo-specific and lives under
`evaluation/locomo/backends/mem0/`; reusable mem0 SDK helpers should live under
`evaluation/clients/` instead.

## Output

The script writes to repository-root `outputs/locomo/<RUN_ID>/<backend>/`.

```text
outputs/locomo/<RUN_ID>/
  ram-a/
    store.sqlite             # RAM-A SQLite hybrid store
    search_results.json      # raw retrieval results
    retrieval_metrics.json   # evidence-hit metrics
    responses.json           # generated answers
    judge_results.json       # LLM judge, BLEU, and F1 results
    qa_metrics.json          # aggregate QA metrics
    report.html              # combined HTML report
    errors.html              # failed-question details
    run_meta.json            # run configuration and git hash
    stage_reports/

  mem0/
    storage/                 # mem0 local state
    search_results.json
    retrieval_metrics.json   # stub diagnostic report; no turn-path evidence
    responses.json
    judge_results.json
    qa_metrics.json
    report.html
    errors.html
    run_meta.json
    stage_reports/
```

Full output directories are generated artifacts. Do not commit them. Put long-lived
artifacts in external storage and add a compact record to `evaluation/baselines/index.jsonl`.

## Primary Metrics

- `llm_score`: main LoCoMo QA accuracy signal from LLM judge.
- `f1_score` and `bleu_score`: lexical overlap diagnostics.
- `evidence_hit_at_k` and `evidence_mrr`: retrieval evidence diagnostics for RAM-A.
- `avg_total_tokens` and latency fields: answer-stage cost and speed diagnostics.

Category 5 adversarial or unanswerable questions are excluded from the main QA score. Use a
separate abstention rubric if those questions become part of the target metric.

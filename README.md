# RAM-A

RAM-A is a standalone long-term memory module for add/search experiments and benchmark
baselines. The repository currently focuses on a local Rust workspace, a command-line
benchmark runner, and reproducible evaluation scripts.

## Scope

- `memory-core`: core long-term memory API and storage implementations.
- `memory-bench`: CLI runner for ingesting benchmark records and searching top-k memories.
- Default local storage: SQLite.
- Default retrieval mode: hybrid dense embedding retrieval plus BM25 text retrieval.
- Embedding providers: OpenRouter-compatible embeddings for real runs and a deterministic
  hash embedding provider for offline smoke tests.

## Repository Layout

```text
crates/
  memory-core/       # core memory API, record model, stores, retrieval
  memory-bench/      # benchmark CLI

evaluation/
  common/            # shared evaluation helpers, metrics, reports, backends
  backends/          # optional benchmark comparison backends
  clients/           # optional third-party benchmark clients
  fixtures/          # small checked-in smoke datasets
  datasets/          # dataset acquisition and local placement notes
  baselines/         # Git-friendly baseline index format
  longmemeval/       # LongMemEval adapter
  personalmem/       # PersonaMem adapter
  scripts/           # dataset-specific orchestration scripts

docs/
  benchmarks/        # benchmark schema and result conventions
  design/            # design notes and implemented architecture decisions
  guides/            # task-oriented usage guides
```

Large raw datasets, generated stores, HTML reports, and full run artifacts are not part of
the source tree. Keep them under `data/`, `outputs/`, `artifacts/`, or external object
storage and record reproducible summaries in `evaluation/baselines/`.

## Core API

`memory-core` exposes the minimal long-term memory interface:

```rust
#[async_trait::async_trait]
pub trait LongTermMemory: Send + Sync {
    async fn add(&self, request: AddMemoryRequest) -> MemoryResult<AddMemoryResponse>;
    async fn search(&self, request: SearchMemoryRequest) -> MemoryResult<Vec<ScoredMemory>>;
}
```

Add request:

```rust
pub struct AddMemoryRequest {
    pub id: Option<String>,
    pub text: String,
    pub metadata: serde_json::Value,
}
```

Search request:

```rust
pub struct SearchMemoryRequest {
    pub query: String,
    pub top_k: usize,
    pub filter: Option<serde_json::Value>,
}
```

## Quick Start

Use the checked-in fixture for an offline smoke test:

```powershell
cargo run -p memory-bench -- `
  --store data/sample.sqlite `
  --embedding hash `
  add `
  --dataset evaluation/fixtures/sample.json

cargo run -p memory-bench -- `
  --store data/sample.sqlite `
  --embedding hash `
  search `
  --dataset evaluation/fixtures/sample.json `
  --output outputs/sample_results.json
```

Use OpenRouter embeddings for a real local retrieval run:

```powershell
$env:OPENROUTER_API_KEY="your_openrouter_key"

cargo run -p memory-bench -- `
  --store data/memory.sqlite `
  --embedding openrouter `
  --model baai/bge-m3 `
  --dimensions 1024 `
  add `
  --dataset data/locomo/locomo10.json `
  --text-fields text,content,message,memory

cargo run -p memory-bench -- `
  --store data/memory.sqlite `
  --embedding openrouter `
  --model baai/bge-m3 `
  --dimensions 1024 `
  search `
  --dataset data/locomo/locomo10.json `
  --query-fields question,query `
  --top-k 10 `
  --output outputs/bge_m3_top10.json
```

Do not commit API keys, local stores, downloaded datasets, or generated reports.

## Evaluation

Start from [evaluation/README.md](evaluation/README.md) for the benchmark overview.

The current evaluation entry points are:

- PersonaMem: [evaluation/personalmem/README.md](evaluation/personalmem/README.md)
- LongMemEval: [evaluation/longmemeval/README.md](evaluation/longmemeval/README.md)
- LoCoMo: [evaluation/locomo/README.md](evaluation/locomo/README.md)

Benchmark-prepared schema guidance lives in
[docs/benchmarks/prepared-schema-v1.md](docs/benchmarks/prepared-schema-v1.md).
Baseline result storage guidance lives in
[evaluation/baselines/README.md](evaluation/baselines/README.md).

## Verification

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
PYTHONPATH=evaluation python -m pytest evaluation
```

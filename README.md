# RAM-A

RAM-A is a standalone long-term memory module for add/search experiments and benchmark
baselines. The repository currently focuses on a local Rust workspace, a command-line
benchmark runner, and reproducible evaluation scripts.

## Scope

- `memory-core`: core long-term memory API and storage implementations.
- `memory-bench`: CLI runner for ingesting benchmark records and searching top-k memories.
- `memory-cases`: local case knowledge service for storing, indexing, and retrieving case documents.
- Default local storage: SQLite.
- Default retrieval mode: hybrid dense embedding retrieval plus BM25 text retrieval.
- Embedding providers: OpenAI-compatible embeddings, including self-hosted `/v1/embeddings`
  services, for real runs and a deterministic hash embedding provider for offline smoke tests.

## Repository Layout

```text
crates/
  memory-core/       # core memory API, record model, stores, retrieval
  memory-bench/      # benchmark CLI
  memory-cases/      # case API, ingestor, chunk store, and search index

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

## Dataset Sources

Full benchmark datasets are not committed to this repository. Use the upstream links
below and keep local downloads under `data/`:

| Dataset | Source / download | Local placement |
|---------|-------------------|-----------------|
| PersonaMem | [GitHub](https://github.com/bowen-upenn/PersonaMem), [HuggingFace](https://huggingface.co/datasets/bowen-upenn/PersonaMem) | `data/personalmem/raw/` and `data/personalmem/prepared/` |
| LongMemEval | [GitHub](https://github.com/xiaowu0162/longmemeval), [cleaned oracle JSON](https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned/resolve/main/longmemeval_oracle.json) | `data/longmemeval/longmemeval_oracle.json` |
| LoCoMo | [GitHub](https://github.com/snap-research/locomo), [locomo10.json](https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json) | `data/locomo/locomo10.json` |

The repository only keeps small synthetic or truncated fixtures under
`evaluation/fixtures/` for smoke tests.

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

## memory-cases Storage

`memory-cases` keeps durable case library data separate from the derived search index:

- Case data store: SQLite tables for datasets, uploaded documents, ingestion tasks, and chunks.
- Search index store: `memory-core` SQLite index for indexed text, FTS rows, and embeddings.
  Document index records are scoped with `memory_index_namespace = "memory-cases"` so update
  and delete flows do not remove other memories in the same DB.
- Chunk sizing uses tiktoken `cl100k_base` token counting.

See [docs/design/memory-cases-storage-split.md](docs/design/memory-cases-storage-split.md) for
table ownership and index semantics.

`memory-cases` defaults to local hash embeddings for offline and demo use. Use
`--embedding-provider openai_compatible` with `--embedding-base-url`, `--embedding-model`,
`--embedding-api-key-env`, and `--embedding-dimensions` when a real embedding service is
available. The endpoint must be OpenAI-compatible and expose `/v1/embeddings`; this includes
self-hosted embedding services.

Keep the case-library search index in a separate SQLite file from RAM-A personal long-term
memory for demos and deployments. Sharing one SQLite index is only suitable for narrow smoke
tests because two services can contend on SQLite writer locks and case reindex/reset flows
should not touch user memories. If a shared `memory-core` SQLite index is intentionally used,
use one embedding profile per search scope. RAM-A stores embedding profile metadata on new
records and rejects same-scope profile mismatches, because two providers can have the same
vector dimensions but incompatible semantic spaces.

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

## HTTP MCP server

`memory-mcp` provides `ram-a-mcp-server`, a Streamable HTTP MCP server for
long-term memory search, ingest, and tenant-authorized case-library retrieval.
It exposes `/mcp`, `/healthy`, and `/ready`, requires bearer-token
authentication, and stores personal memory plus idempotency state in one
SQLite WAL database file. Case documents remain in the separate
`memory-cases` stores.

### Minimal RAM-A + MCP client deployment

The runtime has three separate configuration surfaces:

- RAM-A server config: controls auth, HTTP limits, storage, model providers, and optional
  case-library routing.
- MCP client config, commonly `.mcp.json`: tells an MCP client how to connect to RAM-A.
- Client application config, for example xiaoO `config.toml`: controls the client's own LLM
  provider and optional automatic memory behavior.

Create `config/ram-a-mcp.local.json`:

Copyable examples are also available under [`plugins/mcp/`](plugins/mcp/), including
[`plugins/mcp/ram-a-mcp.local.json`](plugins/mcp/ram-a-mcp.local.json),
[`plugins/mcp/xiaoo.mcp.json`](plugins/mcp/xiaoo.mcp.json), and
[`plugins/mcp/xiaoo-config.toml`](plugins/mcp/xiaoo-config.toml). For client-side prompt
guidance, see [`plugins/mcp/case-tool-instruction.md`](plugins/mcp/case-tool-instruction.md).
xiaoO + RAM-A knowledge base configuration can start from those files and then apply the
field changes listed below.

```json
{
  "auth": {
    "tokens": [
      {
        "token_env": "RAM_A_XIAOO_TOKEN",
        "tenant_id": "tenant-local",
        "user_id": "alice",
        "agent_id": "xiaoo",
        "permissions": ["memory:read", "memory:write", "cases:read"]
      }
    ]
  },
  "http": {
    "bind_address": "127.0.0.1",
    "port": 18081,
    "allowed_origins": ["http://127.0.0.1:18080"],
    "allowed_hosts": ["127.0.0.1:18081"]
  },
  "limits": {
    "max_body_bytes": 1048576,
    "requests_per_second": 20,
    "rate_burst": 40,
    "max_in_flight_per_principal_tool": 4,
    "initialize_requests_per_second": 4,
    "initialize_rate_burst": 8,
    "max_active_sessions_per_principal": 8,
    "max_active_sessions_global": 256,
    "session_idle_timeout_seconds": 1800
  },
  "storage": {
    "database_path": "data/ram-a-memory.sqlite"
  },
  "providers": {
    "api_key_env": "LLM_API_KEY",
    "base_url": "http://127.0.0.1:8000/v1",
    "embedding_provider": "hash",
    "embedding_model": "hash",
    "embedding_dimensions": 1024,
    "extractor_model": "GLM-5.2",
    "verifier_model": "GLM-5.2",
    "timeout_seconds": 120,
    "max_retries": 3
  },
  "case_service": {
    "base_url": "http://127.0.0.1:18082",
    "bearer_token_env": "MEMORY_CASES_API_TOKEN",
    "timeout_seconds": 5,
    "max_response_bytes": 262144,
    "default_library": "ops",
    "libraries": [
      {
        "name": "ops",
        "dataset_id": "openeuler-ops-cases",
        "tenant_ids": ["tenant-local"]
      }
    ]
  }
}
```

Change these fields before deployment:

- `RAM_A_XIAOO_TOKEN`: environment variable name that contains the RAM-A bearer token.
- `tenant_id`, `user_id`, `agent_id`: identity scope for memory isolation.
- `permissions`: include `memory:read`, `memory:write`, and optionally `cases:read`.
- `allowed_hosts`: host and port used by clients to reach RAM-A.
- `storage.database_path`: RAM-A personal long-term memory SQLite path.
- `providers.api_key_env`, `providers.base_url`, `extractor_model`, `verifier_model`:
  OpenAI-compatible chat/completions provider used by extraction and verification.
- `embedding_provider`: use `hash` for local demos; use `openai_compatible` for a real
  embedding service.
- `case_service`: omit this block if the case-library tool is not needed.

For real embedding retrieval, replace the hash embedding fields with an OpenAI-compatible
embedding provider:

```json
{
  "embedding_provider": "openai_compatible",
  "embedding_api_key_env": "EMBEDDING_API_KEY",
  "embedding_base_url": "http://127.0.0.1:8001/v1",
  "embedding_model": "local-embedding-model",
  "embedding_dimensions": 1024
}
```

Start RAM-A:

```bash
export RAM_A_XIAOO_TOKEN='replace-with-long-random-token'
export MEMORY_CASES_API_TOKEN='replace-with-internal-case-token'
export LLM_API_KEY='replace-with-llm-provider-key'

cargo run -p memory-mcp --bin ram-a-mcp-server -- \
  --config config/ram-a-mcp.local.json
```

If `case_service` is enabled, start `memory-cases` separately. Keep the case-library
business DB, case-library index DB, and RAM-A personal memory DB as three different files:

```bash
export MEMORY_CASES_API_TOKEN='replace-with-internal-case-token'

cargo run -p memory-cases -- --api \
  --bind 127.0.0.1:18082 \
  --rag-store data/memory-cases.sqlite \
  --memory-store data/memory-cases-index.sqlite \
  --embedding-provider hash \
  --embedding-dimensions 1024
```

Use the same `--rag-store` and `--memory-store` when running the case ingestor. Do not point
`--memory-store` at `data/ram-a-memory.sqlite`; that can cause SQLite writer contention and
case index resets must not touch personal long-term memories.

### MCP client config

For xiaoO, create `.mcp.json`; see
[`plugins/mcp/xiaoo.mcp.json`](plugins/mcp/xiaoo.mcp.json) for a copyable example. Other MCP
clients may use a different config file shape, but they need the same connection values:
Streamable HTTP transport, `/mcp` URL, bearer token, agent ID if supported, timeout, and MCP
protocol version `2025-11-25`.

```json
{
  "mcpServers": {
    "ram-a": {
      "transport": "streamable_http",
      "url": "http://127.0.0.1:18081/mcp",
      "bearer_token_env": "RAM_A_XIAOO_TOKEN",
      "agent_id": "xiaoo",
      "timeout_ms": 30000
    }
  }
}
```

Change these fields:

- `url`: RAM-A MCP endpoint. RAM-A currently exposes MCP only at `/mcp`.
- `bearer_token_env`: client-side environment variable containing the same token configured
  in RAM-A `auth.tokens[*].token_env`.
- `agent_id`: must match RAM-A `auth.tokens[*].agent_id` when `X-Agent-ID` is sent.
- `timeout_ms`: increase this if memory extraction or provider calls are slow.

Do not put `Authorization`, `Origin`, `X-Agent-ID`, `mcp-session-id`, or
`mcp-protocol-version` into static MCP headers. A compliant Streamable HTTP MCP client should
manage them per request/session. Non-xiaoO clients must still send bearer auth and negotiate
MCP protocol version `2025-11-25`.

After initialization, clients can discover RAM-A tools with `tools/list`. The expected tools
are:

- `memory_search`: search authenticated personal long-term memory.
- `memory_ingest`: extract, ground, and persist authenticated conversation memory.
- `memory_case_search`: first-use tool for troubleshooting, incident diagnosis, root-cause
  analysis, remediation steps, operational case lookup, or similar historical case questions.
  It searches an authorized case library when `case_service` is configured and the token has
  `cases:read`.

If the client model does not reliably choose the case-library tool by itself, add the prompt
snippet in [`plugins/mcp/case-tool-instruction.md`](plugins/mcp/case-tool-instruction.md) to
the client's system prompt, role prompt, or demo prompt. This is a client-side guidance issue;
RAM-A still exposes the tool through standard `tools/list` and does not force a call.

For xiaoO automatic memory, add this to xiaoO `config.toml`:

```toml
[memory_automation]
enabled = true
server = "ram-a"
recall_top_k = 5
recall_token_budget = 512
context_messages = 4
queue_path = "memory-automation-queue.jsonl"
queue_capacity = 256
max_retries = 5
retry_backoff_ms = 250
allowed_agent_roles = ["main", "defaultagent"]
```

This setting enables pre-turn recall and post-turn durable ingest queueing. It does not
modify the user's original prompt. If RAM-A is unavailable, xiaoO should degrade memory
behavior and continue normal chat.

Check the services:

```bash
curl -i http://127.0.0.1:18081/healthz
curl -i http://127.0.0.1:18081/readyz
curl -i http://127.0.0.1:18082/health
```

See [docs/guides/http-mcp-deployment.md](docs/guides/http-mcp-deployment.md)
for secure local deployment, xiaoO `.mcp.json`, automatic memory settings,
health checks, and the current single-instance boundary.
See
[docs/guides/xiaoo-case-library-integration.md](docs/guides/xiaoo-case-library-integration.md)
for the `memory_case_search` contract and the xiaoO integration boundary.

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

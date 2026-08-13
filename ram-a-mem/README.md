# RAM-A

RAM-A is a standalone long-term memory module for add/search experiments and benchmark
baselines. The repository currently focuses on a local Rust workspace, a command-line
benchmark runner, and reproducible evaluation scripts.

## Scope

- `memory-core`: core long-term memory API and storage implementations.
- `memory-bench`: CLI runner for ingesting benchmark records and searching top-k memories.
- `memory-cases`: embeddable case-library engine for storing, indexing, and retrieving case documents.
- Default local storage: SQLite.
- Default retrieval mode: hybrid dense embedding retrieval plus BM25 text retrieval.
- Embedding providers: OpenAI-compatible embeddings, including self-hosted `/v1/embeddings`
  services, for real runs and a deterministic hash embedding provider for offline smoke tests.

## Repository Layout

```text
crates/
  memory-core/       # core memory API, record model, stores, retrieval
  memory-bench/      # benchmark CLI
  memory-cases/      # embeddable case API, ingestion worker, chunk store, and search index

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
    pub graph_memory_space_id: Option<String>,
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

`memory-cases` defaults to local hash embeddings for offline and demo use. Configure
`case_library.embedding_provider` as `openai_compatible` together with its base URL, model,
API key environment name, and dimensions when a real embedding service is available. The
endpoint must be OpenAI-compatible and expose `/v1/embeddings`; this includes self-hosted
embedding services.

Keep the case-library search index in a separate SQLite file from RAM-A personal long-term
memory for demos and deployments. Sharing one SQLite index is only suitable for narrow smoke
tests because independent storage operations can contend on SQLite writer locks and case
reindex/reset flows should not touch user memories. If a shared `memory-core` SQLite index is intentionally used,
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

Graph memory benchmark mode is opt-in. `--graph-build` builds the graph during `add`;
`--graph` enables the graph retrieval channel during `search`. Graph extraction uses an
OpenAI-compatible chat-completions endpoint; by default it reads the same
`OPENROUTER_API_KEY` environment variable and uses OpenRouter.

```powershell
$env:OPENROUTER_API_KEY="your_openrouter_key"

cargo run -p memory-bench -- `
  --store data/locomo_graph.sqlite `
  --embedding openrouter `
  --model baai/bge-m3 `
  --dimensions 1024 `
  --graph-build `
  --graph-llm-model openai/gpt-4o-mini `
  add `
  --dataset data/locomo/locomo10.json `
  --text-fields text,content,message,memory

cargo run -p memory-bench -- `
  --store data/locomo_graph.sqlite `
  --embedding openrouter `
  --model baai/bge-m3 `
  --dimensions 1024 `
  --graph `
  --graph-weight 0.2 `
  search `
  --dataset data/locomo/locomo10.json `
  --query-fields question,query `
  --top-k 10 `
  --output outputs/locomo_graph_top10.json
```

In graph `auto` memory-space mode, prepared-schema datasets use `scope_id`, while raw
top-level-array datasets use the top-level JSON path such as `path:$[0]`. Keep graph and
baseline runs in separate SQLite files when comparing scores.

For ad-hoc graph search with `--query`, pass the memory space through `--filter`, for example
`--filter '{"scope_id":"scope-a"}'`; otherwise no graph memory space can be inferred from the
single query path. With `--resume --graph-build`, existing MemoryRecords are still checked for
graph build: completed graph runs are skipped, missing graph runs are built, and failed/running
graph runs fail explicitly instead of being treated as successful.

Do not commit API keys, local stores, downloaded datasets, or generated reports.

## RAM-A memory MCP service

`memory-mcp` provides `ram-a-mem`, a single Streamable HTTP MCP service for
personal long-term memory and tenant-authorized case-library retrieval. It exposes
`/mcp`, `/healthy`, and `/ready` on one configured HTTP port. The service name is
`ram-a-mem` so it can be distinguished from future services such as `ram-a-kv`.

### Minimal RAM-A + MCP client deployment

The runtime has three configuration surfaces:

- RAM-A memory service config: controls auth, HTTP limits, personal memory storage,
  model providers, and optional embedded case-library storage.
- MCP client config, commonly `.mcp.json`: tells an MCP client how to connect to RAM-A.
- Client application config, for example xiaoO `config.toml`: controls the client's own LLM
  provider and optional automatic memory behavior.

By default, `ram-a-mem` looks for config in this order:

1. `--config <path>`
2. `RAM_A_MEM_CONFIG`
3. `./config/ram-a-mem.json`
4. `~/.config/ram-a/ram-a-mem.json`
5. `/etc/ram-a/ram-a-mem.json`

Create `config/ram-a-mem.json`:

Copyable examples are also available under [`plugins/mcp/`](plugins/mcp/), including
[`plugins/mcp/ram-a-mem.json`](plugins/mcp/ram-a-mem.json),
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
  "features": {
    "memory": {
      "enabled": true
    },
    "case_library": {
      "enabled": true
    },
    "graph_memory": {
      "enabled": false
    }
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
  "case_library": {
    "rag_store": "data/memory-cases.sqlite",
    "index_store": "data/memory-cases-index.sqlite",
    "source_dir": "crates/memory-cases/test/accuracy_docs",
    "api_token_env": "RAM_A_CASES_ADMIN_TOKEN",
    "ingestion_poll_ms": 1000,
    "embedding_provider": "hash",
    "embedding_model": "hash",
    "embedding_dimensions": 1024,
    "chunk_size": 160,
    "default_library": "ops",
    "libraries": [
      {
        "name": "ops",
        "dataset_id": "openeuler-ops-cases",
        "tenant_ids": ["tenant-local"]
      }
    ]
  },
  "graph_memory": {
    "llm_api_key_env": "GRAPH_LLM_API_KEY",
    "llm_base_url": "http://127.0.0.1:8000/v1",
    "llm_model": "GLM-5.2",
    "llm_timeout_ms": 60000,
    "build_concurrency": 1,
    "retrieval": {
      "weight": 0.2,
      "rerank_with_graph": false,
      "allow_graph_only": false,
      "max_graph_only_results": null,
      "seed_limit": null,
      "max_evidence_records_per_fact": null,
      "fail_open": false
    }
  }
}
```

Change these fields before deployment:

- `RAM_A_XIAOO_TOKEN`: environment variable name that contains the RAM-A bearer token.
- `tenant_id`, `user_id`, `agent_id`: identity scope for memory isolation.
- `permissions`: include `memory:read`, `memory:write`, and optionally `cases:read`.
  Grant `cases:write` only to trusted MCP principals allowed to prepare and confirm case writes.
- `features.memory.enabled`: expose or hide RAM-A personal long-term memory tools
  (`memory_search`, `memory_ingest`).
- `features.case_library.enabled`: expose or hide the case-library tools
  (`memory_case_search`, all `memory_case_prepare_*` tools, and the upload, update, and delete
  confirmation tools). If this is `true`, `case_library` must also be configured.
- `features.graph_memory.enabled`: augment `memory_ingest` and `memory_search` with graph
  construction and retrieval. Keep this `false` unless the top-level `graph_memory` settings
  and `GRAPH_LLM_API_KEY` are configured.
- `allowed_hosts`: host and port used by clients to reach RAM-A.
- `storage.database_path`: RAM-A personal long-term memory SQLite path.
- `providers.api_key_env`, `providers.base_url`, `extractor_model`, `verifier_model`:
  OpenAI-compatible chat/completions provider used by extraction and verification.
- `providers.embedding_provider`: use `hash` for local demos; use `openai_compatible` for
  real semantic memory retrieval.
- `case_library.rag_store`: case-library business SQLite path for datasets, documents,
  tasks, chunks, and uploaded source files.
- `case_library.index_store`: case-library retrieval index SQLite path. Keep it separate
  from `storage.database_path`.
- `case_library.source_dir`: optional local directory of `.md`/`.txt` documents. When set,
  `ram-a-mem` imports new files into the default case-library dataset on startup.
- `case_library.api_token_env`: optional environment variable containing the dedicated
  administrator token for the case-management REST API. Omit it to keep that API disabled.
- `case_library.ingestion_poll_ms`: polling interval for the ingestion worker embedded in
  `ram-a-mem`. The worker continuously processes document create/update tasks.
- `case_library.embedding_provider`: case-library retrieval embedding provider. It can be
  `hash` for demos or `openai_compatible` for a real/self-hosted embedding service.

For real embedding retrieval, replace the hash embedding fields with an OpenAI-compatible
embedding provider in `providers` or `case_library` as needed:

```json
{
  "embedding_provider": "openai_compatible",
  "embedding_api_key_env": "EMBEDDING_API_KEY",
  "embedding_base_url": "http://127.0.0.1:8001/v1",
  "embedding_model": "local-embedding-model",
  "embedding_dimensions": 1024
}
```

Start RAM-A memory service:

```bash
export RAM_A_XIAOO_TOKEN='replace-with-long-random-token'
export RAM_A_CASES_ADMIN_TOKEN='replace-with-a-separate-admin-token'
export LLM_API_KEY='replace-with-llm-provider-key'
# Required only when features.graph_memory.enabled is true.
export GRAPH_LLM_API_KEY='replace-with-graph-provider-key'

cargo run -p memory-mcp --bin ram-a-mem
```

The command above reads `config/ram-a-mem.json` by default. To use another path:

```bash
RAM_A_MEM_CONFIG=/etc/ram-a/ram-a-mem.json ram-a-mem
```

Do not point `case_library.index_store` at `storage.database_path`; case index rebuilds and
personal long-term memories must remain isolated even though both capabilities are served
from the same `ram-a-mem` process and HTTP port.

When `case_library.api_token_env` is configured, `ram-a-mem` also serves the authenticated
case-management API under `/api/v1`. Creating or updating a document enqueues an ingestion
task; the background worker in the same process parses, chunks, and indexes it. Do not start
a separate `memory-cases --api` or `memory-cases --ingestor` process.

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
  Also use it when the user asks whether there were similar past cases, previous incidents,
  known fixes, or examples for a troubleshooting symptom, even if they do not explicitly say
  "case library" or name the tool.
  It searches an authorized case library when `case_library` is configured and the token has
  `cases:read`.
- `memory_case_prepare_upload`: after diagnosis, stage a proposed UTF-8 Markdown/text case
  without writing it, and return a proposal plus a one-time confirmation token.
- `memory_case_prepare_update`: stage a proposed replacement for an existing case without
  writing it, and return the same confirmation information.
- `memory_case_prepare_delete`: stage the proposed deletion of an existing case, including a
  required deletion reason, without removing anything.
- `memory_case_upload`: consume a prepared upload token after explicit user confirmation and
  return a generated ingestion task ID.
- `memory_case_update`: consume a prepared update token after explicit user confirmation;
  the replacement is indexed asynchronously by the in-process ingestion worker.
- `memory_case_delete`: consume a prepared delete token after explicit user confirmation and
  immediately remove the document, source file, tasks, chunks, and search records.

All six mutation tools require `cases:write`. Preparation accepts `library` rather than
`dataset_id`, so the server keeps dataset selection and tenant authorization under configuration
control. After diagnosis, prepare an upload with arguments such as:

```json
{
  "library": "ops",
  "document_id": "dns-case-001",
  "file_name": "dns-failure.md",
  "name": "DNS failure recovery",
  "diagnosis_summary": "The local resolver held a stale record; flushing it restored DNS.",
  "content": "# DNS failure\n\nFlush the resolver cache and verify the upstream DNS server."
}
```

Prepare a deletion with the exact document ID and a reason that xiaoO can show to the user:

```json
{
  "library": "ops",
  "document_id": "dns-case-001",
  "deletion_reason": "This case is obsolete and its remediation is no longer safe."
}
```

The preparation call does not modify the case library. The client must display its proposal and
ask the user, then end the turn. Only after a later explicit confirmation may it call the matching
final tool with `{"confirmation_token":"...","user_confirmed":true}`. Tokens expire after ten
minutes, are bound to tenant/user/agent and operation, are single-use, and are lost on service
restart. Upload and update return `ingestion_status: "pending"`, and the background worker in
`ram-a-mem` processes those tasks automatically. Delete completes synchronously and returns
`deleted: true`. `case_library.api_token_env` is not required for these MCP tools—it only
controls the separate REST management API.

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
curl -i http://127.0.0.1:18081/healthy
curl -i http://127.0.0.1:18081/ready
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

# RAM-A memory MCP deployment guide

RAM-A exposes personal long-term memory and tenant-authorized case-library retrieval
over Streamable HTTP MCP through the `ram-a-mem` service. The implemented MCP protocol
version is `2025-11-25`.

The service exposes one HTTP port:

- `POST /mcp`
- `DELETE /mcp`
- `GET /healthy`
- `GET /ready`

`memory_case_search`, `memory_search`, and `memory_ingest` are MCP tools on the same
`/mcp` endpoint. Do not start a separate `memory-cases` API for normal RAM-A memory
deployment.

## Config lookup

`ram-a-mem` reads config in this order:

1. `--config <path>`
2. `RAM_A_MEM_CONFIG`
3. `./config/ram-a-mem.json`
4. `~/.config/ram-a/ram-a-mem.json`
5. `/etc/ram-a/ram-a-mem.json`

## Server configuration

Create `config/ram-a-mem.json`:

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
  "retrieval": {
    "mode": "hybrid",
    "candidate_k": 100,
    "embedding_weight": 0.7,
    "bm25_weight": 0.3,
    "rerank": {
      "enabled": false,
      "provider": "openrouter",
      "model": "cohere/rerank-v3.5",
      "api_key_env": "OPENROUTER_API_KEY",
      "base_url": "https://openrouter.ai/api/v1",
      "input_k": 40,
      "timeout_ms": 30000,
      "fail_open": false
    }
  },
  "case_library": {
    "rag_store": "data/memory-cases.sqlite",
    "index_store": "data/memory-cases-index.sqlite",
    "source_dir": "crates/memory-cases/test/accuracy_docs",
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
  }
}
```

Set secrets in the environment, not in config files:

```bash
export RAM_A_XIAOO_TOKEN='replace-with-a-long-random-token'
export LLM_API_KEY='replace-with-provider-key'
# Required only when features.graph_memory.enabled is true.
export GRAPH_LLM_API_KEY='replace-with-graph-provider-key'
cargo run -p memory-mcp --bin ram-a-mem
```

When running an installed binary:

```bash
ram-a-mem
```

Use a non-default config path when needed:

```bash
RAM_A_MEM_CONFIG=/etc/ram-a/ram-a-mem.json ram-a-mem
```

## Feature switches

- `features.memory.enabled` controls `memory_search` and `memory_ingest`.
- `features.case_library.enabled` controls `memory_case_search`.
- `features.graph_memory.enabled` augments `memory_ingest` and `memory_search` with graph
  construction and retrieval. It does not add a separate MCP tool.

When a feature is disabled, the corresponding tools are hidden from `tools/list`.
If a client still calls a disabled tool directly, RAM-A returns a structured disabled
tool error.

If `features.case_library.enabled` is `true`, `case_library` must be configured.
If `features.case_library.enabled` is omitted, RAM-A enables `memory_case_search` only
when `case_library` is present.

Graph memory is disabled by default. When enabled, `graph_memory` is required and the
LLM credential is read from the configured environment variable. Graph records use the
same principal-scoped SQLite database as personal memories; case-library storage remains
separate.

```json
{
  "features": {
    "memory": {"enabled": true},
    "graph_memory": {"enabled": true}
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

To enable the configuration above, set the graph provider credential and switch the feature on:

```bash
export GRAPH_LLM_API_KEY='replace-with-graph-provider-key'
# Change features.graph_memory.enabled to true in the JSON configuration.
```

Graph retrieval settings use these defaults and validation limits:

- `weight`: `0.2`, must be between `0` and `1`.
- `rerank_with_graph`, `allow_graph_only`, `fail_open`: `false`.
- `max_graph_only_results`: unset means at most 20% of `top_k`, rounded up; when set it must
  be non-zero and is capped by `top_k` at query time.
- `seed_limit`: unset means `max(top_k * 10, 30)`; configured values must be non-zero and are
  capped at `5000`.
- `max_evidence_records_per_fact`: unset means `3`; configured values must be non-zero and are
  capped at `100`.

`features.memory.enabled` must also remain `true`: graph memory augments the existing memory
  ingest and search tools. The supported combinations are `memory=true, graph=false` for the
  ordinary vector/BM25 path and `memory=true, graph=true` for graph augmentation; enabling graph
  while disabling memory is rejected. There is no per-tool graph switch.

The graph feature is fail-closed at the API boundary by default: an enabled graph build or
retrieval failure returns a retriable memory error instead of a successful response with
incomplete graph behavior. Ingest uses a recoverable staged write rather than a distributed
transaction. Atomic memories are persisted before graph augmentation starts, so a graph-build
error does not roll back the atomic memories. Repeating the same idempotent ingest resumes the
missing graph work without duplicating the atomic memories.

Graph construction currently runs synchronously before a successful ingest response.
`build_concurrency` bounds concurrent graph embedding and LLM extraction work during one ingest
request; SQLite operations remain short, serialized transactions. Run one RAM-A service process
per SQLite database. Multi-process workers sharing one database require a future persisted lease
and heartbeat protocol. Deployments that need ingest latency independent of the graph provider
should put ingestion behind an application queue; the current service deliberately favors a
completed graph on every successful response. Because graph construction is synchronous, a reverse
proxy or load balancer in front of this endpoint must use request/read timeouts longer than the
configured graph LLM timeout plus its retry backoff. SSE keep-alive settings do not replace that
request timeout requirement for a tool call that is still being processed.

Provider base URLs are trusted operator configuration and intentionally support self-hosted
services on loopback or private networks. Use HTTPS for remote providers and protect configuration
write access: RAM-A sends the configured API credential to that endpoint.

## Model and embedding providers

`providers` configures the LLM used by personal memory extraction and verification.
`providers.embedding_provider` configures personal memory retrieval embeddings.

`case_library.embedding_provider` configures case-library retrieval embeddings.
It is intentionally part of the same `ram-a-mem` config file; users should not pass
case-library stores or embedding settings as command-line flags.

Supported embedding providers:

- `hash`: deterministic local hash embeddings. Use for demos, offline smoke tests, and
  environments where semantic recall quality is not the focus.
- `openai_compatible`: call `{base_url}/embeddings` with the configured model. Use for
  OpenRouter or a self-hosted OpenAI-compatible embedding service.

Example self-hosted embedding settings:

```json
{
  "embedding_provider": "openai_compatible",
  "embedding_api_key_env": "EMBEDDING_API_KEY",
  "embedding_base_url": "http://127.0.0.1:8001/v1",
  "embedding_model": "local-embedding-model",
  "embedding_dimensions": 1024
}
```

## Retrieval and rerank

`retrieval.mode` supports `dense`, `bm25`, and `hybrid` in the MCP service. `hybrid` is the
default. `graph` is intentionally configured through `features.graph_memory` and
`graph_memory.retrieval`, so there is only one graph-memory switch and configuration source.

For hybrid retrieval, `embedding_weight` and `bm25_weight` must each be between `0` and `1`
and must sum to `1`. `candidate_k`, when set, must be between `1` and `500`. Learned rerank is
optional and is applied after hybrid fusion. It therefore requires `mode=hybrid`.

The first rerank adapter uses the OpenRouter-compatible wire protocol. `provider=openrouter`
describes that HTTP protocol and does not require the service itself to be hosted by OpenRouter.
RAM-A sends `POST {base_url}/rerank` (or uses `base_url` directly when it already ends in
`/rerank`) with this shape:

```json
{
  "model": "local-rerank-model",
  "query": "the search query",
  "documents": ["candidate one", "candidate two"],
  "top_n": 2
}
```

The endpoint must return indexes into the original `documents` array:

```json
{
  "results": [
    {"index": 1, "relevance_score": 0.93},
    {"index": 0, "relevance_score": 0.71}
  ]
}
```

For OpenRouter or another authenticated endpoint, set `api_key_env` to the environment
variable holding the Bearer credential. Authenticated public endpoints must use HTTPS; plain HTTP
is accepted only for loopback, RFC1918/unique-local, or link-local hosts. An unauthenticated local service may set
`api_key_env` to `null`; RAM-A then omits the `Authorization` header. Setting it to `null` is an
explicit operator acknowledgement: expose that endpoint only on a trusted loopback, container,
or private network. A local inference server using another request or response schema needs a
protocol adapter or gateway.

`fail_open=false` makes rerank errors fail the search. With `fail_open=true`, RAM-A returns the
pre-rerank hybrid order and emits a `ram_a.memory.search.degraded` event.

## Structured progress logs

`ram-a-mem` writes one-line JSON logs. Use `RUST_LOG` to select the level; the default is `info`.
Every MCP tool call carries the HTTP `request_id` in its tracing span. Memory ingest emits stage
events for validation, idempotency, normalization, episode/window construction, extraction,
verification, vector persistence, optional graph build, and completion. Hybrid search emits
stage events for query embedding, dense retrieval, BM25 retrieval, fusion, optional graph
augmentation, rerank, filtering, and response completion.

Each stage uses one of these stable event names:

- `ram_a.memory.ingest.stage.started|completed|failed`
- `ram_a.memory.search.stage.started|completed|failed`
- `ram_a.provider.retry|failed`
- `ram_a.case.ingestion.started|completed|failed`
- `ram_a.case.ingestion.task.started|completed|failed`

The latest event for a `request_id` shows the currently running or last failed stage. Events
include elapsed time and counts where available. Logs never include API credentials, query text,
memory text, or provider response bodies. Successful ingest events include generated record IDs;
case task events include `task_id`, `dataset_id`, and `document_id` for operational correlation.

## Storage boundary

Keep these SQLite files separate:

- `storage.database_path`: personal long-term memory and idempotency state.
- `case_library.rag_store`: case datasets, documents, ingestion tasks, chunks, and
  uploaded source files.
- `case_library.index_store`: case-library retrieval index.

Do not point `case_library.index_store` at `storage.database_path`. Case-library reindexing
and personal long-term memories must remain isolated even though both capabilities are
served by the same `ram-a-mem` process and HTTP port.

If `case_library.source_dir` is configured, `ram-a-mem` imports new `.md`, `.markdown`,
`.mdx`, `.txt`, `.text`, and `.log` files from that directory into the default case-library
dataset on startup. Existing documents with the same file name are skipped.

## Network boundary

The default bind address is loopback. Keep RAM-A on localhost for development.
If binding to a non-loopback address, put the service behind TLS termination,
configure an external `allowed_hosts` entry, and set
`tls_termination_acknowledged = true`.

Browser requests carrying `Origin` are accepted only when the origin is in
`allowed_origins`. Non-browser local clients may omit `Origin`. All MCP requests require
`Authorization: Bearer ...`.

## xiaoO `.mcp.json`

Point xiaoO at the single RAM-A MCP endpoint:

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

xiaoO treats `Origin`, `Authorization`, `X-Agent-ID`, `mcp-session-id`, and
`mcp-protocol-version` as transport-managed headers. Do not put them in `.mcp.json`
static headers.

## xiaoO automatic memory

Enable xiaoO automatic recall/ingest separately in `config.toml`:

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

Recall is injected as bounded untrusted system context before a turn. xiaoO does not
rewrite the user's original message. After a successful turn, ingest is written to a
durable retry queue; RAM-A outages degrade memory behavior but must not prevent a normal
xiaoO reply.

## Health checks

```bash
curl -i http://127.0.0.1:18081/healthy
curl -i http://127.0.0.1:18081/ready
```

`GET /healthy` verifies that the HTTP process is alive. `GET /ready` verifies that
dependencies are constructed, session capacity is available, and the SQLite memory schema
is initialized.

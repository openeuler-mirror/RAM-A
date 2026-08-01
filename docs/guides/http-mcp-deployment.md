# RAM-A HTTP MCP deployment guide

RAM-A exposes long-term memory over Streamable HTTP MCP at `POST /mcp`.
The implemented protocol version is `2025-11-25`. Health endpoints are
available at `GET /healthz` and `GET /readyz`.

This guide is for secure local or single-host deployment. The current server
uses one SQLite database file with WAL and is intended to run as a single
service instance. Do not run multiple RAM-A MCP server processes against the
same SQLite path, and do not claim horizontal scaling until a separate
coordination layer exists.

## Server configuration

Create a JSON config file for `ram-a-mcp-server`:

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
    "api_key_env": "OPENROUTER_API_KEY",
    "base_url": "https://openrouter.ai/api/v1",
    "embedding_provider": "openai_compatible",
    "embedding_api_key_env": "OPENROUTER_API_KEY",
    "embedding_base_url": "https://openrouter.ai/api/v1",
    "embedding_model": "baai/bge-m3",
    "embedding_dimensions": 1024,
    "extractor_model": "openai/gpt-4.1-mini",
    "verifier_model": "openai/gpt-4.1-mini",
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

Set secrets in the environment, not in config files:

```bash
export RAM_A_XIAOO_TOKEN='replace-with-a-long-random-token'
export MEMORY_CASES_API_TOKEN='replace-with-a-separate-internal-token'
export OPENROUTER_API_KEY='replace-with-provider-key'
cargo run -p memory-mcp --bin ram-a-mcp-server -- --config config/ram-a-mcp.json
```

`providers.embedding_provider` supports:

- `openai_compatible`: call `{base_url}/embeddings` with `embedding_model`. Use this for
  OpenRouter or a self-hosted OpenAI-compatible embedding service.
- `hash`: use deterministic local hash embeddings. This is useful for offline smoke tests and
  demos, but does not provide production semantic recall quality.

`open_router` is accepted as a backwards-compatible alias for `openai_compatible`.
When omitted, `embedding_api_key_env` and `embedding_base_url` fall back to
`api_key_env` and `base_url`. Set them explicitly when chat/extraction uses one
provider but embeddings use a separate self-hosted service.

Each token maps to exactly one `tenant_id`, `user_id`, and `agent_id`.
Search scope is tenant plus user, so multiple agent tokens for the same
tenant/user can share memory while different users remain isolated. A request
that includes `X-Agent-ID` must match the token's configured `agent_id`.

`case_service` is optional. When present, `memory_case_search` maps the
caller-visible library name to a private `memory-cases` dataset ID. The mapping
also restricts each library to the listed tenants. xiaoO cannot submit a
dataset ID. Start the case API with the same internal token in its environment:

```bash
export MEMORY_CASES_API_TOKEN='replace-with-a-separate-internal-token'
cargo run -p memory-cases -- --api \
  --bind 127.0.0.1:18082 \
  --rag-store data/memory-cases.sqlite \
  --memory-store data/memory-cases-index.sqlite \
  --embedding-provider hash \
  --embedding-dimensions 1024
```

`memory-cases` uses the same `memory-core` embedding abstraction as RAM-A memory search,
but the recommended deployment keeps the case-library index DB separate from the RAM-A
long-term memory DB. Use `data/memory-cases-index.sqlite` for case retrieval and
`data/ram-a-memory.sqlite` for personal long-term memory. This avoids SQLite write-lock
contention between two independently running services and keeps case reindex/reset
operations away from user memories.

If you intentionally point `memory-cases` and `ram-a-mcp-server` at the same
`memory-store` SQLite file for a small smoke test, they must use the same embedding
provider, base URL, model, key environment name, and dimensions for each shared search
scope. `memory-core` records the embedding profile on new writes and rejects same-scope
profile mismatches, because equal vector dimensions do not make two different embedding
models semantically compatible. Shared SQLite index deployment is not recommended for
concurrent demo or production services.
For a real or self-hosted embedding service, start both the API and ingestor with matching
embedding settings:

```bash
export LOCAL_EMBEDDING_API_KEY='replace-with-provider-key-or-dummy-if-local-service-ignores-auth'
cargo run -p memory-cases -- --api \
  --bind 127.0.0.1:18082 \
  --rag-store data/memory-cases.sqlite \
  --memory-store data/memory-cases-index.sqlite \
  --embedding-provider openai_compatible \
  --embedding-api-key-env LOCAL_EMBEDDING_API_KEY \
  --embedding-base-url http://127.0.0.1:8000/v1 \
  --embedding-model local-embedding-model \
  --embedding-dimensions 1024
```

Run the ingestor as a separate process against the same two stores and the same
embedding provider/model/dimension settings; otherwise newly ingested records and
queries may use incompatible vector dimensions or semantics. Keep the
case API on loopback; every `/api/v1/*` request requires the internal bearer
token, while `/health` remains available for local liveness checks.

## Network boundary

The default bind address is loopback. Keep RAM-A on localhost for development.
If binding to a non-loopback address, put the service behind TLS termination,
configure an external `allowed_hosts` entry, and set
`tls_termination_acknowledged = true`.

Browser requests carrying `Origin` are accepted only when the origin is in
`allowed_origins`. Non-browser local clients may omit `Origin`. All MCP
requests require `Authorization: Bearer ...`.

Only these endpoints are supported:

- `POST /mcp`
- `DELETE /mcp`
- `GET /healthz`
- `GET /readyz`

Legacy SSE-only transport, stdio transport, and draft `Mcp-Method` /
`Mcp-Name` headers are not implemented.

## xiaoO `.mcp.json`

Point xiaoO at the RAM-A endpoint with Streamable HTTP:

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

xiaoO treats `Origin`, `Authorization`, `X-Agent-ID`,
`mcp-session-id`, and `mcp-protocol-version` as transport-managed headers, so
do not put them in `.mcp.json` `headers`. A local non-browser xiaoO client
normally omits `Origin`; if you introduce a browser or proxy path that sends
one, add that actual origin to RAM-A `allowed_origins`.

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
allowed_agent_roles = ["main"]
```

Recall is injected as bounded untrusted system context before a turn. xiaoO
does not rewrite the user's original message. After a successful turn, ingest
is written to a durable retry queue; RAM-A outages degrade memory behavior but
must not prevent a normal xiaoO reply.

The existing memory automation settings apply to personal `memory_search` and
`memory_ingest`; they do not automatically call the case library. A
model-driven xiaoO MCP client can discover and choose `memory_case_search`
without a xiaoO code change. Deterministic pre-turn case recall requires a
separate xiaoO orchestration change. See
[xiaoo-case-library-integration.md](xiaoo-case-library-integration.md) for the
tool contract, recognition behavior, and data locations.

## Operations

- `GET /healthz` verifies the HTTP process is alive.
- `GET /readyz` verifies dependencies are constructed, session capacity is
  available, and the SQLite schema is present.
- SQLite WAL is used through the shared RAM-A store/idempotency database file.
- Personal memory and case-library data use separate SQLite files; do not point
  `memory-mcp` storage at either `memory-cases` store.
- Logs must not include bearer token values or provider credentials.
- The service performs LLM extraction and grounding with the configured
  provider credentials. Offline tests use static fixtures and do not call live
  model providers.

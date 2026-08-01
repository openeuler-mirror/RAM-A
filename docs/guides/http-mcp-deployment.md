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

When a feature is disabled, the corresponding tools are hidden from `tools/list`.
If a client still calls a disabled tool directly, RAM-A returns a structured disabled
tool error.

If `features.case_library.enabled` is `true`, `case_library` must be configured.
If `features.case_library.enabled` is omitted, RAM-A enables `memory_case_search` only
when `case_library` is present.

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

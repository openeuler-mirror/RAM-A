<div align="center">

# RAM-A

**Reasoning Aware Memory infrastructure for AI agents**

A monorepo of two independent Rust workspaces — long-term memory and KV cache coordination — designed to give conversational AI agents durable, retrievable, and low-latency memory.

[![License: MulanPSL-2.0](https://img.shields.io/badge/License-MulanPSL--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2021%20edition-orange.svg)](https://www.rust-lang.org/)
[![Status](https://img.shields.io/badge/status-active%20development-green.svg)](#)

</div>

---

## Overview

RAM-A provides the memory substrate for multi-turn AI agent workflows:

- **Long-term memory** (`ram-a-mem`) — durable, semantic, retrievable memory across sessions.
- **KV cache coordination** (`ram-a-kv`) — event-driven, semantically-aware proactive scheduling of KV cache for multi-turn agent inference.

The two workspaces are **independent** and can be built, deployed, and versioned separately.

> **Why it matters:** In agent loops, each turn depends on prior intermediate results, but existing KV cache systems (e.g. LMCache-Ascend) are *passive warehouse managers* — LRU-only, blind to semantics. Under multi-tenant pressure, hot KV cache gets evicted to slow storage and TTFT spikes from milliseconds to seconds. `ram-a-kv` acts as a *semantic brain*: it maps Agent context to KV chunks, predicts reuse, and proactively orchestrates prefetch / eviction / demotion **before** the next inference — compressing TTFT back to milliseconds.

## Highlights

| | Capability |
|---|---|
| 🧠 **Long-term memory** | Minimal add/search API, hybrid retrieval (dense embeddings + BM25), optional graph memory |
| 📚 **Case library** | Embeddable case store with ingest, chunk, and retrieval pipeline |
| 🌐 **MCP service** | Streamable HTTP MCP server for agent integration |
| ⚡ **KV cache daemon** | Event-driven proactive prefetch / eviction / demotion of KV chunks based on Agent behavior; cross-session reference counting |
| 🧩 **TypeScript plugin** | OpenClaw plugin for KV cache integration |
| 📊 **Reproducible benchmarks** | LoCoMo / LongMemEval / PersonaMem evaluation harness |
| 🦀 **Pure Rust core** | 2021 edition, workspace-based, `clippy -D warnings` clean |

## Repository Layout

```text
ram-a-mem/                     # Long-term memory workspace
  crates/
    memory-core/               # core memory store, retrieval, graph
    memory-bench/              # benchmark CLI harness
    memory-cases/              # embeddable case library
    memory-mcp/                # Streamable HTTP MCP service
    memory-pipeline/           # ingestion / extraction pipeline
  evaluation/                  # benchmarks, adapters, baselines
  docs/                        # design docs and guides

ram-a-kv/                      # KV cache coordination workspace (semantic brain)
  crates/
    ram-a-kv/                  # event-driven daemon: turn_start/end, session_fork, snapshot_restore...
    manager-core/              # backend manager abstraction (LMCache-Ascend adapter)
    ram-a-kv-sdk/              # Rust SDK
  openclaw-plugin/             # OpenClaw TypeScript plugin
  docs/                        # design docs and guides
```

## Quick Start

### ram-a-mem

#### 1. Offline smoke test (no API key)

Uses built-in hash embeddings so you can verify the pipeline locally without any external service:

```bash
cd ram-a-mem

# add documents to the memory store
cargo run -p memory-bench -- --store data/sample.sqlite --embedding hash \
  add --dataset evaluation/fixtures/sample.json

# run retrieval and write results
cargo run -p memory-bench -- --store data/sample.sqlite --embedding hash \
  search --dataset evaluation/fixtures/sample.json --output outputs/sample_results.json
```

#### 2. Real retrieval run (OpenAI-compatible embeddings)

Point `memory-bench` at any OpenAI-compatible `/v1/embeddings` endpoint (e.g. OpenRouter, a self-hosted `bge-m3` service, etc.) for real semantic retrieval:

```bash
cd ram-a-mem
export OPENROUTER_API_KEY="your_openrouter_key"

# ingest with a real embedding model
cargo run -p memory-bench -- \
  --store data/memory.sqlite \
  --embedding openrouter \
  --model baai/bge-m3 \
  --dimensions 1024 \
  add \
  --dataset data/locomo/locomo10.json \
  --text-fields text,content,message,memory

# search top-k and write results
cargo run -p memory-bench -- \
  --store data/memory.sqlite \
  --embedding openrouter \
  --model baai/bge-m3 \
  --dimensions 1024 \
  search \
  --dataset data/locomo/locomo10.json \
  --query-fields question,query \
  --top-k 10 \
  --output outputs/bge_m3_top10.json
```

Graph memory is opt-in: add `--graph-build` during `add` and `--graph` during `search` (uses an OpenAI-compatible chat endpoint for extraction).

#### 3. Deploy the MCP service

`memory-mcp` ships `ram-a-mem`, a single Streamable HTTP MCP service exposing personal long-term memory and authorized case-library retrieval at `/mcp`, `/healthy`, `/ready`.

```bash
cd ram-a-mem

# minimal config lookup order:
#   --config <path>  >  RAM_A_MEM_CONFIG  >  ./config/ram-a-mem.json
#                    >  ~/.config/ram-a/ram-a-mem.json  >  /etc/ram-a/ram-a-mem.json
mkdir -p config
# copy and edit auth tokens, storage paths, embedding provider, case_library...
cp crates/memory-mcp/config.example.json config/ram-a-mem.json

export RAM_A_XIAOO_TOKEN='replace-with-long-random-token'
export LLM_API_KEY='replace-with-llm-provider-key'

cargo run -p memory-mcp --bin ram-a-mem
```

Connect from any Streamable HTTP MCP client (e.g. xiaoO `.mcp.json`):

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

Clients then discover tools via `tools/list`: `memory_search`, `memory_ingest`, `memory_case_search`, and the `memory_case_prepare_*` / `memory_case_*` confirmation workflow. See the [ram-a-mem README](ram-a-mem/README.md) for the full config schema, case-library, and graph-memory options.

### ram-a-kv — build and run the daemon

```bash
cd ram-a-kv
cargo build --release
RAM_A_KV_CONFIG=/path/to/config.toml ./target/release/ram-a-kv
```

## Documentation

| Topic | Location |
|-------|----------|
| ram-a-mem overview, core API, MCP deployment | [ram-a-mem/README.md](ram-a-mem/README.md) |
| ram-a-mem design docs and guides | [ram-a-mem/docs/](ram-a-mem/docs/README.md) |
| Benchmarks (LoCoMo / LongMemEval / PersonaMem) | [ram-a-mem/evaluation/README.md](ram-a-mem/evaluation/README.md) |
| ram-a-kv usage guide (architecture, events, SDK) | [ram-a-kv/docs/usage-guide.md](ram-a-kv/docs/usage-guide.md) |

## Contributing

Run checks from the workspace you changed:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`ram-a-kv` also ships `./tests/test_integration.sh`.

> **Note:** Never commit API keys, local stores, datasets, or generated reports.

## License

Distributed under the [Mulan PSL v2](LICENSE).

Third-party notices:
- [ram-a-mem](ram-a-mem/Third_Party_Open_Source_Software_Notice.md)
- [ram-a-kv](ram-a-kv/Third_Party_Open_Source_Software_Notice.md)

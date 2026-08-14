# RAM-A MCP example configs

This directory contains copyable example configuration files for deploying RAM-A over
Streamable HTTP MCP and connecting an MCP client.

- [`ram-a-mem.json`](ram-a-mem.json): RAM-A memory service config for local demo or
  single-host deployment.
- [`xiaoo.mcp.json`](xiaoo.mcp.json): xiaoO MCP client config example.
- [`xiaoo-config.toml`](xiaoo-config.toml): xiaoO application config snippet with automatic
  RAM-A memory recall/ingest enabled.
- [`case-tool-instruction.md`](case-tool-instruction.md): optional prompt snippet that tells
  the client model to call `memory_case_search` first for troubleshooting and case lookup
  questions, and to wait for explicit user confirmation before a prepared case is written.

Deployment steps and field-by-field notes are documented in
[`../../README.md#minimal-ram-a--mcp-client-deployment`](../../README.md#minimal-ram-a--mcp-client-deployment).

For xiaoO + RAM-A knowledge base integration, use these examples as the starting point and
replace all token environment names, identity fields, model provider settings, host allowlists,
and SQLite paths for your environment.

In `ram-a-mem.json`, `features.memory.enabled` controls the personal long-term
memory tools and `features.case_library.enabled` controls the case-library search/mutation
tools. When the case-library feature is enabled, keep `case_library` configured and grant
`cases:read` to clients that search. Grant `cases:write` only to trusted MCP management clients
that may prepare and confirm uploads, updates, or deletes. The xiaoO example has `cases:write`
because it demonstrates this workflow: a `memory_case_prepare_*` tool stages an in-memory
proposal without mutating the library; after xiaoO shows it and the user explicitly confirms in
a later turn, the matching final tool consumes the one-time token and performs the operation.

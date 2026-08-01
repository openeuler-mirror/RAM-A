# RAM-A MCP example configs

This directory contains copyable example configuration files for deploying RAM-A over
Streamable HTTP MCP and connecting an MCP client.

- [`ram-a-mcp.local.json`](ram-a-mcp.local.json): RAM-A server config for local demo or
  single-host deployment.
- [`xiaoo.mcp.json`](xiaoo.mcp.json): xiaoO MCP client config example.
- [`xiaoo-config.toml`](xiaoo-config.toml): xiaoO application config snippet with automatic
  RAM-A memory recall/ingest enabled.
- [`case-tool-instruction.md`](case-tool-instruction.md): optional prompt snippet that tells
  the client model to call `memory_case_search` first for troubleshooting and case lookup
  questions.

Deployment steps and field-by-field notes are documented in
[`../../README.md#minimal-ram-a--mcp-client-deployment`](../../README.md#minimal-ram-a--mcp-client-deployment).

For xiaoO + RAM-A knowledge base integration, use these examples as the starting point and
replace all token environment names, identity fields, model provider settings, host allowlists,
and SQLite paths for your environment.

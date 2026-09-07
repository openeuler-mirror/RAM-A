# Provider Reasoning Effort Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the memory pipeline's chat completion reasoning effort configurable and select `none` for the GLM deployment.

**Architecture:** Extend the existing provider configuration with an optional pass-through string. Configure the shared `OpenAiCompatibleClient` once at startup so both extraction and grounding requests receive the same conditional payload field.

**Tech Stack:** Rust, serde/serde_json, reqwest, Tokio tests

---

### Task 1: Define request payload behavior

**Files:**
- Modify: `ram-a-mem/crates/memory-pipeline/src/client.rs`

- [ ] Add a local HTTP-server test asserting a configured client sends `"reasoning_effort":"none"`.
- [ ] Run `cargo test -p memory-pipeline client::tests::configured_reasoning_effort_is_sent -- --exact` and confirm it fails because the client API does not yet support the option.
- [ ] Add an optional client field and a builder that trims blank input to no override, then conditionally insert the JSON field.
- [ ] Add and run a compatibility test asserting an unconfigured client omits the field.

### Task 2: Wire server configuration

**Files:**
- Modify: `ram-a-mem/crates/memory-mcp/src/config.rs`
- Modify: `ram-a-mem/crates/memory-mcp/src/main.rs`
- Modify: `ram-a-mem/crates/memory-mcp/tests/http_mcp.rs`

- [ ] Add a failing configuration test for deserializing `providers.reasoning_effort` and rejecting a blank value.
- [ ] Add `Option<String>` to `ProvidersConfig`, validate non-blank configured values, and pass it to the model client builder.
- [ ] Run the focused memory-mcp configuration tests and update explicit struct literals.

### Task 3: Select the GLM deployment setting

**Files:**
- Modify: `ram-a-mem/plugins/mcp/ram-a-mem.json`
- Modify: `ram-a-mem/plugins/mcp/xiaoo-config.toml`

- [ ] Set the RAM-A field in the deployment config and document it as `null` in the generic bundled example.
- [ ] Replace xiaoO's unsupported `off` value with `none`; make the GLM smoke request use `none`, require final `content`, and retry transient HTTP failures.

### Task 4: Full and live verification

**Files:** None

- [ ] Run rustfmt checks on the touched Rust files and the relevant Rust workspace tests. Record unrelated platform-specific baseline failures separately.
- [ ] Review the diff, commit the feature, and push to the existing PR branch.

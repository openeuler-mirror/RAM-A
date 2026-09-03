# Provider reasoning effort design

## Goal

Allow RAM-A memory extraction and grounding requests to pass an optional OpenAI-compatible `reasoning_effort` field. The ARM64 GLM Coding Plan image will set it to `none` so short or structured completions are less likely to exhaust the completion budget before returning `message.content`.

## Configuration and compatibility

Add `providers.reasoning_effort` as an optional string. Existing configurations that omit it retain the exact current request body. When present, the value must not be blank and is copied unchanged into each extraction and verification chat-completion request. RAM-A does not restrict values to GLM's current enum because other OpenAI-compatible providers may support different values.

## Data flow

`ServerConfig` deserializes and validates the option. `ram-a-mem` passes it to `OpenAiCompatibleClient`; the client conditionally inserts it into the JSON payload shared by the extractor and verifier. The ARM64 deployment config selects `none`.

The bundled xiaoO GLM configuration also uses `none`; its previous `off` value is rejected by the Coding Plan endpoint. The live smoke request sends the same setting and requires a non-empty final `content`, while accepting an optional string `reasoning_content` alongside it.

## Verification

Unit tests capture a real local HTTP request and verify both compatibility paths: configured clients include `reasoning_effort`, while unconfigured clients omit it. Configuration tests verify deserialization and reject blank values. Asset tests verify the ARM64 image configuration. Finally, rebuild RAM-A in the ARM64 container and run the live GLM-backed memory ingestion check. The smoke request retries transient HTTP failures a bounded number of times; persistent Coding Plan usage-limit errors remain a reported external verification blocker.

Follow-up design: `../plans/2026-09-03-memory-model-compatibility-phase-1.zh-CN.md` expands this pass-through field into typed provider compatibility controls, reasoning-only classification, bounded JSON repair, and per-stage token budgets.

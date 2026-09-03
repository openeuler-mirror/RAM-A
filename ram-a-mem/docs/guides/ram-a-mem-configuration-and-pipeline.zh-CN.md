# RAM-A-MEM 完整配置与记忆管线

本文以 `ram-a-mem` HTTP MCP 服务的当前实现为准。仓库中的完整配置文件是
[`plugins/mcp/ram-a-mem.json`](../../plugins/mcp/ram-a-mem.json)。该文件显式列出了
`ServerConfig` 的全部配置模块和字段；密钥只写环境变量名称，不写密钥值。
逐字段的默认值、推荐值、约束、生效条件和测试要求见
[`ram-a-mem-configuration-reference.zh-CN.md`](ram-a-mem-configuration-reference.zh-CN.md)。
RPM 和 xiaoO 的可执行环境验收步骤见
[`ram-a-mem-rpm-agent-self-test.zh-CN.md`](ram-a-mem-rpm-agent-self-test.zh-CN.md)。

## 配置生效规则

- 配置文件采用 JSON，并拒绝未知字段。
- `auth`、持久化 `storage` 和 `providers` 是生产启动必需项。
- `features.*.enabled` 决定对应模块是否对外提供能力；配置对象存在不代表功能已开启。
- `graph_memory` 仅在 `features.graph_memory.enabled=true` 时参与摄入和检索。
- `case_library` 仅在案例库功能开启时构建案例库服务。
- `retrieval.rerank` 仅在 `enabled=true` 时调用 Rerank 服务。
- 配置中的 `*_env` 是环境变量名称。实际 Token/API Key 必须通过进程环境注入。

## 配置模块说明

### `auth`

`tokens` 至少包含一个主体绑定。每项中的 `token_env` 指向 Bearer Token 环境变量；
`tenant_id + user_id` 用于派生数据隔离 `scope_id`，`agent_id` 用于可选的
`x-agent-id` 一致性校验。`permissions` 当前支持 `memory:read`、`memory:write`、
`cases:read` 和 `cases:write`。不同配置项不能解析为相同 Token。

### `features`

- `memory.enabled`：控制 `memory_ingest` 和 `memory_search`。
- `case_library.enabled`：控制案例检索和案例变更工具；显式开启时必须提供
  `case_library`。
- `graph_memory.enabled`：在原子记忆基础上启用图构建和图检索；必须同时开启
  `memory` 并提供 `graph_memory`。

### `http`

- `bind_address`、`port`：监听地址和端口。
- `allowed_origins`：允许浏览器跨域访问的 Origin；空数组表示不授予跨域来源。
- `allowed_hosts`：允许的 HTTP Host，不能为空。
- `tls_termination_acknowledged`：非 loopback 地址监听时必须为 `true`，表示部署者已在
  服务前配置 TLS termination；该字段本身不会启用 TLS。

### `limits`

这些限制作用于整个服务，不是 MCP Tool 参数。

| 字段 | 默认值 | 有效范围 | 作用 |
| --- | ---: | ---: | --- |
| `max_body_bytes` | 16777216 | 1..=67108864 | 单个 MCP HTTP 请求体上限 |
| `requests_per_second` | 20 | 1..=10000 | 每主体、每 Tool 的持续速率 |
| `rate_burst` | 40 | 1..=100000 | 每主体、每 Tool 的突发容量 |
| `max_in_flight_per_principal_tool` | 4 | 1..=1024 | 每主体、每 Tool 的并发请求数 |
| `initialize_requests_per_second` | 4 | 1..=1000 | MCP initialize 持续速率 |
| `initialize_rate_burst` | 8 | 1..=10000 | MCP initialize 突发容量 |
| `max_active_sessions_per_principal` | 8 | 1..=1024 | 每主体的活动 Session 数 |
| `max_active_sessions_global` | 256 | 1..=100000 | 进程内活动 Session 总数 |
| `session_idle_timeout_seconds` | 1800 | 1..=86400 | MCP Session 空闲回收时间，同时作用于 RAM-A Admission 和底层 `rmcp` Session Worker |

全局 Session 上限必须不小于单主体上限。并发超限不会排队，直接返回 HTTP 429。

### `pipeline`

- `fail_fast`：默认 `true`，只控制 Extract 和 Ground 的模型调用/协议错误。为
  `true` 时终止整个摄入，本次管线结果不写入正式记忆；为 `false` 时跳过失败窗口，
  继续处理其他窗口。
- `max_memory_chars`：默认 500，有效范围 1..=32000，按 Unicode 字符计数。超长抽取
  结果进入 quarantine，不截断，也不使请求失败。
- `max_candidate_tokens`、`max_window_tokens`：Window 阶段的候选预算和候选加上下文预算。
- `extractor_max_output_tokens`、`verifier_max_output_tokens`：Extract/Ground Chat 请求的
  输出 token 上限。
- `extractor_context_window_tokens`、`verifier_context_window_tokens`、`reasoning_reserve_tokens`：
  可选的完整请求预算预检。Extract 超预算时先裁剪非候选上下文；候选内容仍超预算则失败。

Episode 参数目前没有暴露为服务配置。服务固定使用代码默认值：Episode 不按时间间隔和
metadata 字段主动切分；Window 向前取 2 条上下文、向后取 0 条上下文。这里的 Token 是本地
启发式估算，不是模型 Tokenizer 的精确计数。

### `storage`

`database_path` 是正式记忆、幂等记录和可选图数据使用的持久化 SQLite 文件。生产配置
不接受空路径或 `:memory:`。同一数据库应只由一个 `ram-a-mem` 进程写入。

### `providers`

- `api_key_env`、`base_url`：Extract 和 Ground 共用的 OpenAI-compatible Chat API。
- `extractor_model`：Extract 阶段模型名。
- `verifier_model`：Ground 阶段模型名；可以与 Extract 相同，也可以独立配置。
- `timeout_seconds`、`max_retries`：上述 Chat API 客户端的单次超时和最大尝试次数配置。
- `reasoning_effort`、`enable_thinking`：可选的 thinking/reasoning 控制字段，最多启用一种；
  Provider 不支持时应省略或在 smoke test 后再开启。
- `send_temperature`、`temperature`：是否发送 temperature，以及发送时的数值。
- `output_token_parameter`：选择发送 `max_tokens` 或 `max_completion_tokens`。
- `structured_output`：选择 `prompt_only`、`json_object` 或 `json_schema`。
- `reasoning_only_retry`：只有 `reasoning_content` 但没有最终 `content` 时，最多纠正重试一次。
- `json_repair_attempts`：Extract/Ground 返回非合格 JSON 时，最多请求模型修复一次。
- `embedding_provider`：`hash` 或 `openai_compatible`。`hash` 仅适合离线测试和演示。
- `embedding_api_key_env`、`embedding_base_url`：可选；未配置时回退到 `api_key_env` 和
  `base_url`。
- `embedding_model`、`embedding_dimensions`：正式记忆写入和 dense 检索的向量模型及维度。

即使选择 `embedding_provider=hash`，`api_key_env` 仍然必需，因为 Extract 和 Ground
仍调用 Chat 模型。
模型兼容性字段和 GLM Coding Plan 的已验证注意事项见
[`model-compatibility.zh-CN.md`](model-compatibility.zh-CN.md)。

### `retrieval`

- `mode`：`dense`、`bm25` 或 `hybrid`；MCP 服务不接受独立的 `graph` mode。
- `embedding_weight`、`bm25_weight`：Hybrid 权重，均为 0..=1 且总和必须为 1。
- `candidate_k`：可选固定候选数，范围 1..=500；为 `null` 时使用核心检索的动态规则。
- `rerank.enabled`：是否在 Hybrid 结果上调用 Rerank。
- `rerank.provider`：当前仅支持配置值 `openrouter`，同时兼容同协议的自托管端点。
- `rerank.model`、`api_key_env`、`base_url`：Rerank 模型和端点。`api_key_env=null`
  可用于无需认证的本地端点。
- `rerank.input_k`：送入 Rerank 的结果数，范围 1..=500；运行时至少为 `top_k`。
- `rerank.timeout_ms`：启用时范围 1..=120000。
- `rerank.fail_open`：`false` 时 Rerank 异常返回 `RERANK_FAILED`；`true` 时返回 Rerank
  前的 Hybrid 顺序。

### `case_library`

- `rag_store`：案例源数据、任务和 chunk 的 SQLite 文件。
- `index_store`：案例检索索引 SQLite 文件，必须与 `rag_store` 和个人记忆库不同。
- `source_dir`：可选的本地案例导入目录。
- `api_token_env`：可选的案例管理 REST API 独立管理员 Token 环境变量。
- `ingestion_poll_ms`：内置摄入 Worker 的轮询间隔，必须大于 0。
- `embedding_*`、`chunk_size`：案例 chunk 的向量化配置。
- `summary_llm_*`：可选的案例摘要模型配置；`summary_llm_model=null` 表示不启用模型摘要。
- `default_library`、`libraries`：MCP 暴露的逻辑库名到内部 dataset ID 和允许租户的映射。

### `graph_memory`

- `llm_*`：原子记忆写入后进行图实体/关系抽取的 OpenAI-compatible Chat 模型配置。
- `build_concurrency`：一次摄入请求内的图构建并发数，必须大于 0。
- `retrieval.weight`：图通道参与融合的权重，范围 0..=1。
- `rerank_with_graph`、`allow_graph_only`：是否让图结果参加重排、是否允许纯图结果。
- `max_graph_only_results`、`seed_limit`、`max_evidence_records_per_fact`：`null` 使用代码默认
  规则；显式值必须大于 0。
- `fail_open`：图检索失败时是否退回非图结果。图摄入失败与图检索失败是七阶段之外的行为。

## `memory_ingest` 完整服务链

七阶段只负责“从内部消息形成经过验证的原子记忆”。一次完整的 MCP 摄入还包含协议、认证、
请求校验、幂等、Embedding 和持久化：

```text
HTTP/MCP 解析
  -> Bearer Token 认证和 memory:write 鉴权
  -> request_validate
  -> scope_id / ingest_lock / idempotency_reserve
  -> 构造 prepared JSON
  -> Normalize -> Episode -> Window -> Extract -> Validate -> Ground -> Aggregate
  -> 构造 AddMemoryRequest
  -> Embedding -> vector_persist
  -> 可选 graph_build
  -> idempotency_complete
  -> MCP 响应
```

### 外部输入

`memory_ingest` 的 Tool arguments 为：

```json
{
  "conversation_id": "conv-1",
  "messages": [
    {
      "id": "m1",
      "role": "user",
      "speaker": "Alice",
      "text": "我喜欢喝绿茶。",
      "timestamp": "2026-08-17T10:00:00Z",
      "candidate": true
    }
  ]
}
```

### 七阶段外围的输入输出

| 处理步骤 | 输入 | 输出或副作用 |
| --- | --- | --- |
| MCP 协议解析 | JSON-RPC `tools/call` 请求 | 解析出 `IngestRequest`；未知字段或错误 JSON 在此拒绝 |
| 认证鉴权 | Bearer Token、可选 `x-agent-id` | `Principal(tenant_id,user_id,agent_id,permissions)` |
| `request_validate` | `IngestRequest` | 校验字段长度、消息数量、role、RFC3339 和请求内 ID 唯一性 |
| scope 派生 | Principal | 不透明 `scope_id`，调用者不能自行指定 |
| `ingest_lock` | scope、conversation、candidate message IDs | 同一摄入键的并发请求在进程内串行执行 |
| `idempotency_reserve` | scope、conversation、message ID、content hash | 全部成功过则返回缓存；新记录或 pending 记录进入 `Proceed` |
| prepared 转换 | 通过校验的请求、Principal、待处理 candidate IDs | 内部 `benchmark-prepared-v1` JSON |
| 七阶段 Pipeline | prepared JSON、PipelineConfig、Extractor、Verifier | accepted memories、rejected、quarantined、stats 和运行元数据 |
| Add 请求转换 | Aggregate 输出、Principal、pipeline run ID | `Vec<AddMemoryRequest>` |
| Embedding | accepted memory text | 与配置维度一致的向量；`hash` 为本地计算，其他 Provider 可调用外部服务 |
| `vector_persist` | AddMemoryRequest + embedding | 正式记忆写入 SQLite，并返回 memory IDs |
| `graph_build` | 已接受的原子记忆 | 仅 Graph 开启时构建图；该步骤位于七阶段和原子记忆写入之后 |
| `idempotency_complete` | 本次成功响应 | pending 记录更新为 success 并保存缓存响应 |

`request_validate` 不是七阶段中的 Validate。前者校验外部 MCP 参数，失败返回
`INVALID_REQUEST`，不会进入幂等预占或调用模型；后者校验 Extract 模型产生的原子记忆。

当所有消息都是 `candidate=false` 时，不创建幂等记录，也不调用 Extract/Ground 模型；当前
实现仍可执行 Normalize、Episode、Window 和空 Aggregate，最终返回计数均为 0 的成功响应。

### 外部输出

成功响应的 Tool structured content 为：

```json
{
  "pipeline_run_id": "run-<uuid>",
  "accepted_count": 1,
  "rejected_count": 0,
  "quarantined_count": 0,
  "memory_ids": ["mem-<hash>"],
  "idempotency_hit": false,
  "retriable": false
}
```

响应只公开计数和正式 memory IDs，不公开 rejected/quarantined issue 明细。七阶段发生致命错误
时，本次 accepted 中间结果不会写入正式记忆；在失败前创建的幂等预占记录保持 pending，允许
相同内容重试。已成功写入的历史记忆不受影响。

七阶段成功后的持久化和 Graph 不构成一个跨模块数据库事务。`vector_persist` 成功后如果
`graph_build` 或 `idempotency_complete` 失败，原子记忆可能已经存在，而幂等记录仍是 pending；
相同内容重试依靠稳定 memory ID 做 upsert，并继续未完成工作。因此不能把“Tool 返回失败”一律
理解为“本次请求绝对没有写入任何数据”，只有七阶段内部的致命失败具备前述不写入保证。

## 七阶段与模型调用

| 阶段 | 是否调用模型 | 当前模型来源 | 本阶段输入 | 输出给下一阶段 |
| --- | --- | --- | --- | --- |
| Normalize | 否 | 本地 Rust | prepared JSON | `Vec<NormalizedMessage>` |
| Episode | 否 | 本地 Rust | normalized messages + EpisodeConfig | `Vec<ConversationEpisode>` |
| Window | 否 | 本地 Rust | episodes + message lookup + WindowConfig | `Vec<ExtractionWindow>` |
| Extract | 是 | `providers.extractor_model` | 一个 window + 相关 normalized messages | `ExtractionBatch.raw_memories` |
| Validate | 否 | 本地 Rust | raw memories + window + message lookup + ValidationConfig | valid/rejected/quarantined |
| Ground | 是 | `providers.verifier_model` | valid atomic memories + Evidence 对应消息 | `SUPPORTED` 等验证结果 |
| Aggregate | 否 | 本地 Rust | 所有 `SUPPORTED` atomic memories + source lookup | 去重后的正式 memory records |

Embedding 模型不属于七阶段。它在七阶段成功后写入正式记忆，以及执行 dense 检索时调用。
Rerank、案例摘要和图抽取模型也不属于这七阶段。

## 阶段数据示例

下面使用一条上下文消息和一条候选消息说明数据如何传递。示例中的哈希 ID 用占位符表示，
真实值由 `scope_id + conversation_id + message_id` 等字段稳定计算。

### 0. MCP 请求转换为 prepared JSON

MCP 请求中的内容：

```json
{
  "conversation_id": "conv-1",
  "messages": [
    {"id": "m1", "role": "assistant", "text": "你平时喝什么？", "candidate": false},
    {"id": "m2", "role": "user", "speaker": "Alice", "text": "我喜欢喝绿茶。", "timestamp": "2026-08-17T10:00:00Z", "candidate": true}
  ]
}
```

服务在进入七阶段前生成内部 prepared JSON：

```json
{
  "schema_version": "benchmark-prepared-v1",
  "dataset": {"name": "memory-mcp", "split": "online"},
  "memories": [
    {
      "id": "source-<hash-m1>",
      "text": "你平时喝什么？",
      "metadata": {
        "scope_id": "<principal-scope-hash>",
        "session_id": "session-<hash-conv-1>",
        "role": "assistant",
        "speaker": "assistant",
        "timestamp": "",
        "turn_index": 0,
        "source_agent_id": "xiaoo",
        "memory_candidate": false
      }
    },
    {
      "id": "source-<hash-m2>",
      "text": "我喜欢喝绿茶。",
      "metadata": {
        "scope_id": "<principal-scope-hash>",
        "session_id": "session-<hash-conv-1>",
        "role": "user",
        "speaker": "Alice",
        "timestamp": "2026-08-17T10:00:00Z",
        "turn_index": 1,
        "source_agent_id": "xiaoo",
        "memory_candidate": true
      }
    }
  ],
  "queries": []
}
```

### 1. Normalize

**输入：** 上面的完整 prepared JSON。

**实际检查：** `schema_version` 必须是 `benchmark-prepared-v1`；`memories` 必须是数组；
每条记录应为对象并具有唯一非空 `id`、非空 `text`、对象类型 `metadata` 和非空
`metadata.scope_id`。它还读取 `session_id`、`role`、`speaker`、`timestamp`、
`turn_index` 和 `memory_candidate`。缺少 ID/scope/text 等单条问题形成 Normalize issue 并跳过；
错误 schema、非数组 memories 或重复 ID 是致命错误。

局部问题和致命问题的差别是“是否还能无歧义地处理批次中的其他消息”：

| 输入问题 | 处理方式 | 原因 |
| --- | --- | --- |
| 某条记录不是对象 | 该条形成 `invalid_source_record` issue 并跳过 | 不影响其他记录 |
| 某条缺少 ID、scope 或有效 text | 该条形成 issue 并跳过 | 该消息不能成为可靠来源，但其他消息仍可处理 |
| 某条 metadata 不是对象 | 该条形成 `invalid_source_metadata` issue 并跳过 | 无法提取可信 scope 和上下文信息 |
| schema version 错误 | 整个 Pipeline 失败 | 不能确认输入协议及字段语义 |
| `memories` 不是数组 | 整个 Pipeline 失败 | 不能把输入解释为消息序列 |
| source ID 重复 | 整个 Pipeline 失败 | 后续 Window 和 Evidence 无法确定 ID 指向哪条消息 |

Normalize issue 使用统一的 `PipelineIssue` 结构：

```json
{
  "stage": "normalize",
  "code": "missing_scope_id",
  "message": "source memory is missing metadata.scope_id",
  "source_id": "source-<hash>",
  "scope_id": "",
  "episode_id": "",
  "window_id": "",
  "details": {}
}
```

这些 issue 会进入本次 `PipelineRun.rejected` 并增加 MCP 响应的 `rejected_count`。在线 MCP
服务不持久化 issue 明细，响应和普通日志也不包含明细；请求结束后明细随 PipelineRun 释放。
只有离线调用 `write_pipeline_artifacts` 时，才会写入 `rejected_extractions.jsonl`。

正常 MCP 请求中的空 ID、空 text 等通常已被前面的 `request_validate` 拒绝，因此线上频繁出现
Normalize issue 更可能表示内部 prepared 转换或其他 Pipeline 调用方存在问题。

**输出：**

```json
[
  {"id":"source-<hash-m1>","scope_id":"<scope>","text":"你平时喝什么？","candidate_eligible":false,"role":"assistant","speaker":"assistant","timestamp":"","session_id":"session-<hash>","turn_index":0,"source_index":0,"metadata":{"scope_id":"<scope>","session_id":"session-<hash>","role":"assistant","speaker":"assistant","timestamp":"","turn_index":0,"source_agent_id":"xiaoo","memory_candidate":false}},
  {"id":"source-<hash-m2>","scope_id":"<scope>","text":"我喜欢喝绿茶。","candidate_eligible":true,"role":"user","speaker":"Alice","timestamp":"2026-08-17T10:00:00Z","session_id":"session-<hash>","turn_index":1,"source_index":1,"metadata":{"scope_id":"<scope>","session_id":"session-<hash>","role":"user","speaker":"Alice","timestamp":"2026-08-17T10:00:00Z","turn_index":1,"source_agent_id":"xiaoo","memory_candidate":true}}
]
```

该列表是 Episode 的直接输入；实现中同时建立 `id -> NormalizedMessage` lookup，供 Window、
Extract、Validate、Ground 和 Aggregate 使用。

### 2. Episode

**输入：** ordered normalized messages + EpisodeConfig。它按相邻消息检查 `scope_id`、
`session_id`、配置的 metadata 边界和可选时间间隔。当前 MCP 默认情况下，同一请求的消息具有
相同 scope/session，因此通常形成一个 Episode。

`ordered` 表示保持 MCP 请求中的消息顺序，不按照 timestamp 重新排序。EpisodeConfig 是本地
分组规则，当前 MCP 服务使用以下代码默认值，尚未对外暴露这些字段：

```json
{
  "max_time_gap_minutes": null,
  "metadata_boundary_fields": [],
  "version": "episode_v1"
}
```

相邻消息的 scope 或 session 变化时始终切分；配置 `metadata_boundary_fields` 后，对应 metadata
值发生变化也会切分；配置非负 `max_time_gap_minutes` 后，可解析时间的相邻消息超过该间隔也会
切分。Episode 阶段的结果是对有序消息做连续分区，不会把相隔较远但 scope/session 相同的消息
重新拼接到旧 Episode。

**输出：**

```json
{
  "id": "episode-<hash>",
  "scope_id": "<scope>",
  "session_id": "session-<hash>",
  "message_ids": ["source-<hash-m1>", "source-<hash-m2>"],
  "start_time": "",
  "end_time": "2026-08-17T10:00:00Z",
  "boundary_reason": "start",
  "episode_version": "episode_v1"
}
```

Episode 不复制消息正文，只保存消息 ID 和边界信息。Episode 列表与 message lookup 一起成为
Window 的输入。

### 3. Window

**输入：** episodes、message lookup 和 WindowConfig。它只把 `candidate_eligible=true` 的
span 放入 `candidate_refs`；`false` 消息最多只能进入 context。长消息先按句子和字符 span
切分，再按候选 Token 预算打包，并在总预算内补充前后文。

`candidate_eligible` 不是模型判断结果。在线 MCP 中内部值来自：

```text
candidate_eligible = 请求 message.candidate
                     && 本次幂等预占要求处理该 message ID
```

首次摄入时，`candidate=true` 的消息通常为 eligible；`candidate=false` 的消息只能提供上下文；
字段未提供时默认 true。混合重试中，已经 success 的消息不会再次抽取，但仍可以作为上下文。
离线 prepared 数据缺少 `memory_candidate` 时，为保持旧数据兼容，Normalize 默认按 true 处理。

span 是一条消息内的连续 Unicode 字符区间，使用左闭右开 `[start_char, end_char)` 表示。例如
`start_char=1,end_char=6,text="喜欢喝绿茶"` 表示该片段覆盖第 1 到第 5 个字符。消息超过候选
Token 预算时，会优先按句子边界切成多个 span；无法按句子容纳时继续按字符拆分。

**输出：**

```json
{
  "id": "window-<hash>",
  "scope_id": "<scope>",
  "session_id": "session-<hash>",
  "episode_id": "episode-<hash>",
  "candidate_refs": [{"message_id":"source-<hash-m2>","start_char":0,"end_char":7,"text":"我喜欢喝绿茶。"}],
  "context_before_refs": [{"message_id":"source-<hash-m1>","start_char":0,"end_char":7,"text":"你平时喝什么？"}],
  "context_after_refs": [],
  "candidate_message_ids": ["source-<hash-m2>"],
  "candidate_token_count": 7,
  "total_token_count": 14,
  "window_version": "window_v1"
}
```

当前启发式估算器把每个 CJK 字符和标点分别计为一个 Token。每个 Window 分别进入 Extract。
没有候选 span 时输出空 Window 列表，后续 Extract、Validate、Ground 不执行，Aggregate
产生空结果。

Episode 和 Window 的基数关系为：

```text
Episode 1 -> Window 1, Window 2
Episode 2 -> Window 3
```

一个 Window 只属于一个 Episode，不能由多个 Episode 组成，也不会跨越 Episode 边界。一个
Episode 可以生成零个、一个或多个 Window；每个 Window 包含一组 candidate spans，并可附带
同一 Episode 内的前后 context spans。

### 4. Extract

**输入：** 单个 ExtractionWindow，以及 Window 引用的 NormalizedMessage。模型 Prompt 使用
`<context>` 和 `<candidate>` 分区；上下文只能帮助消歧，不能单独产生记忆。

**模型：** OpenAI-compatible Chat Completions，模型名为 `providers.extractor_model`，当前
实现标识为 `LLMMemoryExtractor`，Prompt 版本 `extract_v3`，最大输出由
`pipeline.extractor_max_output_tokens` 控制，默认 1600 Token。

Extract 请求会按 `providers` 中的模型兼容字段构造请求体。启用上下文窗口预算时，系统会估算
system prompt、user prompt、推理预留和最大输出；如果超出预算，会先移除非候选上下文，再决定
是否调用 Provider。

**模型输出经协议解析后的 ExtractionBatch：**

```json
{
  "window_id": "window-<hash>",
  "schema_version": "atomic_memory_v1",
  "raw_memories": [{
    "text": "Alice 喜欢喝绿茶。",
    "memory_type": "preference",
    "subject": {"name": "Alice", "source_speaker": "Alice"},
    "predicate": "prefers",
    "object": {"name": "绿茶", "type": "drink"},
    "modality": "asserted",
    "event_time": null,
    "attributes": {},
    "evidence": [{"message_id":"source-<hash-m2>","quote":"喜欢喝绿茶","evidence_role":"primary"}],
    "model_confidence": 0.95
  }],
  "usage": {"prompt_tokens":0,"completion_tokens":0,"total_tokens":0,"latency_ms":0},
  "raw_response": "<provider response>"
}
```

`raw_memories` 是 Validate 的输入。此处只验证模型响应是 JSON 对象、schema version 正确且
`memories` 是对象数组；字段语义在下一阶段检查。

### 5. Validate

**输入：** `raw_memories + 当前 window + message lookup + max_memory_chars`。

**实际检查：** 必需字段及 JSON 类型；memory type/modality 枚举；confidence 为 0..=1；
Evidence message 必须在当前 Window；quote 必须在对应 span 中唯一且逐字符匹配；至少一条
primary Evidence 来自 candidate span；记忆字符数不超限；`asserted` 与计划、可能、否定
Evidence 不冲突。

原子记忆字段的当前结构规则为：

| 字段 | 规则 |
| --- | --- |
| `text`、`predicate` | 必需的非空字符串 |
| `subject` | 必需的 JSON 对象 |
| `object` | 可选；只能是 object、string 或 null |
| `memory_type`、`modality` | 必需字符串，并且属于允许枚举 |
| `evidence` | 必需的非空对象数组 |
| `attributes` | 可选对象，缺少时按空对象处理 |
| `event_time` | 可选；只能是对象或 null |
| `model_confidence` | 可选；提供时必须为 0..=1 的数值 |

`model_confidence` 当前只做范围检查，没有“低于某个阈值自动隔离”的规则，也不替代 Ground。
memory type 和 modality 只能验证“值是否属于枚举”，不能通过确定性测试证明模型的语义选择
符合人的主观预期；这部分质量需要标注数据集和评测指标支撑。

每条 Evidence 必须包含 `message_id`、非空 `quote` 和 `primary/supporting` evidence role。
`message_id` 必须存在于当前 Window；quote 必须在该消息被 Window 覆盖的 spans 中逐 Unicode
字符匹配。没有匹配表示模型改写或杜撰了引文；匹配多次表示无法唯一确定字符位置。至少一条
primary Evidence 必须来自 candidate span，context span 只能用于消歧或 supporting Evidence。

**输出：**

```text
ValidationBatch {
  valid: Vec<AtomicMemory>,
  rejected: Vec<PipelineIssue>,
  quarantined: Vec<PipelineIssue>
}
```

未知枚举、字段类型错误、缺少 candidate Evidence 等进入 rejected；Evidence 不精确、超长或
语气可疑进入 quarantine。只有 `valid` 是 Ground 的输入。该阶段不判断模型选择的记忆类型
是否“符合人的主观预期”，只验证值属于枚举和结构规则。

在线 MCP 响应只返回 `rejected_count` 和 `quarantined_count`。当前没有持久化 quarantine 队列、
人工审核或重新放行接口，因此这里的 quarantine 表示“本次请求中隔离且不写入正式记忆”，
不是可供后续审核的长期存储。

### 6. Ground

**输入：** 当前 Window、Validate 通过的 AtomicMemory，以及各 Evidence 对应的
NormalizedMessage。Prompt 只发送候选 claim 和已定位 Evidence，不重新发送任意历史数据。

**模型：** OpenAI-compatible Chat Completions，模型名为 `providers.verifier_model`，当前
实现标识为 `LLMGroundingVerifier`，Prompt 版本 `ground_v2`，最大输出由
`pipeline.verifier_max_output_tokens` 控制，默认 1000 Token。

Ground 同样使用 `providers` 中的模型兼容字段。它只发送候选 claim 和已定位 Evidence，没有
额外外围上下文可裁剪；配置上下文窗口预算后，超预算会作为 Ground 阶段错误返回。

**输出：**

```json
{
  "window_id": "window-<hash>",
  "results": [{"memory_id":"candidate-<hash>","status":"SUPPORTED","reason":"证据完整支持"}],
  "usage": {"prompt_tokens":0,"completion_tokens":0,"total_tokens":0,"latency_ms":0},
  "raw_response": "<provider response>"
}
```

只有 `SUPPORTED` 进入 Aggregate；`PARTIALLY_SUPPORTED`、`UNSUPPORTED`、`UNCERTAIN` 形成
quarantine issue。模型漏掉某个 memory ID 时，该项按 `UNCERTAIN` 处理。

这些值是 Ground 模型给出的证据支持状态，不是服务错误码：

| 状态 | 判定语义 | 示例 |
| --- | --- | --- |
| `SUPPORTED` | Evidence 支持记忆中的所有关键内容 | Evidence 为“我喜欢绿茶”，记忆为“Alice 喜欢绿茶” |
| `PARTIALLY_SUPPORTED` | 只支持部分关键内容 | Evidence 只支持喜欢绿茶，记忆还声称“每天喝三杯” |
| `UNSUPPORTED` | Evidence 不支持或与记忆矛盾 | Evidence 为“我不喜欢绿茶”，记忆为“Alice 喜欢绿茶” |
| `UNCERTAIN` | Evidence 不足、含义模糊或模型漏答，不能可靠判断 | Evidence 为“这个还行”，记忆声称“Alice 最喜欢绿茶” |

Verifier 的 Prompt 要求逐条分类；本地代码验证状态枚举，并把模型漏掉的 ID 补为
`UNCERTAIN`。自动化测试可以验证四种状态对应的程序分流，但实际语义判断质量仍依赖
`providers.verifier_model` 和评测数据。

### 7. Aggregate

**输入：** 所有 Window 中 Ground 为 `SUPPORTED` 的 AtomicMemory，以及 source message
lookup。它补充 observation，按 `scope_id + canonical_content` 精确去重，合并 Evidence 和
observation，并生成稳定的 `mem-<hash>` ID。

`canonical_content` 是参与精确去重的核心字段集合：

```json
{
  "memory_type": "preference",
  "text": "Alice 喜欢绿茶。",
  "subject": {"name": "Alice"},
  "predicate": "prefers",
  "object": {"name": "绿茶", "type": "drink"},
  "modality": "asserted",
  "event_time": null,
  "attributes": {}
}
```

去重键还包含 `scope_id`，但不包含 memory ID、Evidence、observed time、来源 Episode/Window、
model confidence 和 observation refs。因此同一 scope 多次得到完全相同核心内容时会合并来源；
不同用户不会合并。这是结构和字符串级精确去重，不是语义去重，“喜欢绿茶”和“爱喝绿茶”
仍可能形成两条记忆。

**输出：** 去重后的 accepted memories，以及新的 `benchmark-prepared-v1` 输出。服务随后把
accepted memories 转成 `AddMemoryRequest`，调用 Embedding Provider 并写入 SQLite；这一步已在
七阶段之外。

Aggregate 当前没有模型调用，也没有普通 MCP 输入可稳定触发的独立业务失败条件。它的测试
重点是输出构造、稳定 ID、Evidence 合并和精确去重，而不是人为制造模型故障。

## 七阶段日志契约

管线阶段统一使用以下事件和规范阶段名：

```text
event=ram_a.memory.ingest.stage.started   stage=<stage>
event=ram_a.memory.ingest.stage.completed stage=<stage>
event=ram_a.memory.ingest.stage.failed    stage=<stage>
```

阶段名为 `normalize`、`episode`、`window`、`extract`、`validate`、`ground`、`aggregate`。
日志记录计数、耗时、cache hit、错误码及是否可重试，不记录消息、Evidence、模型原始响应或
记忆正文。MCP 参数校验使用独立的 `request_validate` 阶段，避免与七阶段中的 Validate 混淆。

自动化测试覆盖：成功路径七阶段 started/completed 的完整顺序；Normalize、Episode、Window、
Extract、Validate 配置和 Ground 的可触发失败事件；日志不泄露测试消息正文。Aggregate 当前
没有可达的独立失败输入，因此覆盖 started/completed，不伪造不存在的外部故障。

## `memory_search` 完整服务链

检索没有与摄入相同的固定七阶段。它根据 `retrieval.mode`、Graph 和 Rerank 配置选择执行路径，
默认 Hybrid 路径为：

```text
HTTP/MCP 解析
  -> Bearer Token 认证和 memory:read 鉴权
  -> SearchRequest validate
  -> scope_id 过滤和候选池计算
  -> Query Embedding
  -> Dense retrieve + BM25 retrieve
  -> Hybrid fuse
  -> 可选 Graph augment
  -> 可选 Rerank
  -> memory type / event time 后过滤
  -> truncate(top_k)
  -> MCP 响应
```

检索不调用 Extract 或 Ground，不创建幂等记录，不获取 ingest lock，也不修改正式记忆。

### 检索输入

`memory_search` 的 Tool arguments 为：

```json
{
  "query": "Alice 喜欢喝什么？",
  "top_k": 10,
  "memory_types": ["preference"],
  "event_time_from": "2026-01-01T00:00:00Z",
  "event_time_to": "2026-12-31T23:59:59Z"
}
```

参数规则：query 去除空白后非空且最多 32000 个 Unicode 字符；top_k 默认 10、范围
1..=100；memory_types 为空表示不限制，否则只能使用七种记忆类型；时间边界可选，提供时必须
为 RFC3339；请求不接受未定义字段。失败返回 `INVALID_REQUEST`，不进入核心检索。

### 检索阶段输入输出

| 阶段 | 输入 | 输出 | 模型或实现 |
| --- | --- | --- | --- |
| MCP 解析 | JSON-RPC `tools/call` | `SearchRequest` | 本地协议层 |
| 认证鉴权 | Bearer Token、可选 `x-agent-id` | Principal | 本地认证 |
| validate | SearchRequest | 规范化后的过滤条件；非法请求直接失败 | 本地 Rust |
| scope filter | Principal | `{"scope_id":"<hash>"}` | 本地派生，调用者不能覆盖 |
| candidate limit | final top_k | 服务层后过滤候选上限 | 本地 Rust |
| query embedding | query text | 固定维度查询向量 | Hash 或 `providers.embedding_model` |
| dense retrieve | query vector、scope filter、channel limit | `Vec<ScoredMemory>` | SQLite vector cosine |
| BM25 retrieve | 清理后的 query、scope filter、channel limit | `Vec<ScoredMemory>` | SQLite FTS5 BM25 |
| hybrid fuse | Dense/BM25 候选和权重 | 合并、归一化、排序后的候选 | 本地 Rust |
| graph augment | query、scope、Hybrid 候选 | 补充图事实或可选图候选 | SQLite graph；检索时不调用 LLM |
| rerank | query、候选正文 | Rerank 新顺序和分数 | 可选 `retrieval.rerank.model` |
| predicate filter | 候选、memory types、event time 范围 | 过滤后的候选 | 本地 Rust |
| response mapping | 最多 top_k 条候选 | `SearchResponse.memories` | 本地 Rust |

### 检索模式

| `retrieval.mode` | 执行路径 |
| --- | --- |
| `dense` | Query Embedding -> Dense -> 可选 Graph -> 后过滤 |
| `bm25` | BM25 -> 可选 Graph -> 后过滤，不生成 Query Embedding |
| `hybrid` | Query Embedding -> Dense + BM25 -> Hybrid -> 可选 Graph -> 可选 Rerank -> 后过滤 |

MCP 服务不允许把 mode 设置为独立的 `graph`；Graph 是 Dense、BM25 或 Hybrid 的可选增强通道。

### 候选数量

服务为后过滤预取：

```text
service_candidate_limit = min(final_top_k * 5, 500)
```

核心 Hybrid 中每个 Dense/BM25 通道还使用 `retrieval.candidate_k`。当前完整示例显式配置为
100，因此 `top_k=10` 时：

```text
Dense 最多取 100
BM25 最多取 100
Hybrid/Rerank 向服务返回最多 50
后过滤后最终返回最多 10
```

当 `candidate_k=null` 时，核心使用 `max(core_request_top_k * 5, 100)` 的动态规则。这里的
`core_request_top_k` 已是服务层 candidate limit，因此底层单通道查询可能超过 500；最终返回
服务后过滤的候选仍被 `service_candidate_limit` 限制。这是当前实现的两层候选策略，不能把
`top_k * 5` 理解为数据库每个通道的固定扫描上限。

### Dense 输入输出

Query Embedding 使用 `providers.embedding_provider/model/dimensions`。`hash` 在本地计算；
`openai_compatible` 调用配置的 Embedding API。Dense 输入是 query vector 和当前 scope，输出为：

```json
{
  "record": {"id":"mem-1","text":"Alice 喜欢绿茶。","metadata":{}},
  "score": 0.82
}
```

SQLite 使用 cosine distance，并转换为 `dense_score = 1 - distance`，分数越高越相关。查询向量
维度或 embedding profile 与该 scope 已存记忆不一致时失败，不混用不同向量空间。

### BM25 输入输出

BM25 先把 query 按非字母数字字符拆成 FTS token，空 token 查询返回空候选。SQLite BM25 原始
值越低越相关，代码直接转换为 0..=1 的高分优先值：

```text
bm25_score = (max_raw - current_raw) / (max_raw - min_raw)
```

这与“先取反再做 min-max”数学等价；所有原始值相同时统一得到 1.0。BM25 不调用模型。

### Hybrid 输入输出

Hybrid 按 memory ID 合并两个通道，并分别对当前候选集中的已有分数做 min-max：

```text
dense_norm = min_max(dense_score)
bm25_norm  = min_max(bm25_score)
hybrid_score = embedding_weight * dense_norm + bm25_weight * bm25_norm
```

默认权重为 0.7/0.3。某条记忆没有出现在某个通道时，该通道分数按 0；同分时按 memory ID
稳定排序。当前代码不会在整个通道为空时重新归一化权重，例如 BM25 全空时 Dense 排序不变，
但最终分数仍乘以 0.7。因此“空通道权重自动变为 0”不是当前实现的精确描述。

### Graph 输入输出

Graph 开启时使用 query 和当前 scope 从已经持久化的图事实、实体和 Evidence 记录中检索。搜索
阶段不调用图 LLM；图结构已在摄入的 `graph_build` 中生成。默认
`rerank_with_graph=false` 时，Graph 主要给基础候选附加 `graph_facts/graph_matches`，不改变基础
分数。允许图参与排序且 `allow_graph_only=true` 时，才可能加入不在基础候选中的纯图结果。

Graph augment 位于 Rerank 之前。Graph 检索失败时，`graph_memory.retrieval.fail_open=true` 返回
基础候选；false 时检索失败。

### Rerank 输入输出

Rerank 只允许在 Hybrid mode 开启。输入为 query 和候选的 ID/text，Provider 输出相关性分数，
代码据此重排并截断：

```json
{
  "results": [
    {"index": 1, "relevance_score": 0.95},
    {"index": 0, "relevance_score": 0.61}
  ]
}
```

当前 `input_k` 不是固定上限，核心使用 `max(input_k, core_request_top_k)`。例如最终 top_k=10、
服务候选池=50、input_k=40 时，最多会把 50 条而不是固定 40 条送入 Rerank。

`fail_open=false` 时 Provider 超时、可重试 HTTP 错误耗尽重试或协议错误返回
`RERANK_FAILED`；`fail_open=true` 时记录 degraded 日志并返回 Rerank 前的 Hybrid/Graph 顺序。

### 后过滤与最终输出

`memory_types` 和 event time 在 Dense/BM25/Graph/Rerank 之后过滤。时间过滤依次尝试读取
event_time 字符串、`event_time.normalized`、`event_time.raw`；配置时间范围后，没有可解析
event time 的记忆会被排除。因为过滤发生在候选召回之后，最终结果可以少于 top_k，服务不会
继续扩大候选池补足数量。

成功响应为：

```json
{
  "memories": [
    {
      "id": "mem-<hash>",
      "text": "Alice 喜欢绿茶。",
      "memory_type": "preference",
      "modality": "asserted",
      "event_time": null,
      "observed_at": "2026-08-17T10:00:00Z",
      "evidence_refs": [
        {
          "message_id": "source-<hash>",
          "quote": "喜欢喝绿茶",
          "start_char": 1,
          "end_char": 6,
          "evidence_role": "primary"
        }
      ],
      "source_agent_id": "xiaoo",
      "graph_facts": [],
      "graph_facts_truncated": false,
      "score": 0.95
    }
  ]
}
```

无匹配时成功返回空数组。score 不是事实置信度：它可能是 Dense similarity、BM25 归一化值、
Hybrid 融合值或 Rerank relevance score，含义取决于运行配置，不能跨模式直接比较。

### 检索日志与错误

服务层记录 `validate`、`retrieve` 和 `predicate_filter`；默认 Hybrid 核心路径记录
`query_embedding`、`dense_retrieve`、`bm25_retrieve`、`hybrid_fuse`，并按配置增加
`graph_augment` 和 `rerank`。日志记录阶段、候选计数、耗时和错误分类，不记录 query 或记忆
正文。

| 错误 | 当前对外结果 |
| --- | --- |
| 请求参数非法 | `INVALID_REQUEST`，不可重试 |
| Rerank 失败且 fail-open 关闭 | `RERANK_FAILED`，可重试 |
| Embedding、SQLite 或 fail-closed Graph 检索失败 | 当前统一映射为 `STORAGE_FAILED`，可重试 |

最后一项是当前错误映射的实际行为，不表示所有这类故障在语义上都是 SQLite 存储错误。

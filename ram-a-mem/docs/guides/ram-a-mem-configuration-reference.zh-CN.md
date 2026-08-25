# RAM-A-MEM 配置字段参考

本文以 `memory_mcp::config::ServerConfig` 的当前实现为准，覆盖 `ram-a-mem` 服务 JSON
配置的全部字段，并说明代码默认值、部署推荐值、有效范围、生效条件和测试要求。完整配置示例见
[`plugins/mcp/ram-a-mem.json`](../../plugins/mcp/ram-a-mem.json)。

本文字段表中的“测试要求”默认指安装 RPM 后执行的黑盒验收。源码自动化用于提前发现实现
回归，但不能替代 RPM 中二进制、默认目录、运行用户、文件权限和依赖环境的交付验证。

## 1. 字段分类

| 类别 | 含义 | 测试要求 |
| --- | --- | --- |
| 固定值 | 协议、枚举或当前实现只接受列出的值，用户不能任意扩展 | 覆盖全部合法值，并验证未知值启动失败 |
| 推荐可配置 | 允许用户覆盖；表中推荐值是已确认的交付配置 | 验证默认值、推荐值、上下边界和越界值 |
| 部署必配 | 必须由用户按环境填写，不存在跨环境通用值 | 验证必填、非空、格式、引用关系；不编造长度或容量上限 |
| 条件配置 | 仅在对应功能开启或相关字段非空时生效 | 同时验证未启用、正确启用和依赖缺失三种情况 |

除非字段表另有说明，本文的“字符”均不是 MCP Tool 入参长度约束。模型名、文件路径、环境
变量名等没有可靠业务上限，服务只校验当前实现需要的非空、格式或引用关系。

## 2. 通用规则

- 配置格式固定为 JSON，所有配置对象都拒绝未知字段。
- 生产启动要求顶层 `auth`、`storage` 和 `providers` 可用；`auth.tokens` 至少一项。
- `*_env` 的值是环境变量名称，不是 Token 或 API Key 本身。密钥必须通过进程环境注入。
- 顶层可选对象一旦存在，服务仍会校验其字段；功能开关只决定是否构建和对外暴露该能力。
  因此不要在关闭功能时保留一份无效的 `case_library` 或 `graph_memory` 配置。
- 带 API Key 的公网 Provider URL 固定要求 HTTPS；loopback、私网和 link-local 地址允许 HTTP。
- URL 不允许携带用户名、密码、query 或 fragment。
- 配置文件查找优先级固定为：`--config`、`RAM_A_MEM_CONFIG`、
  `config/ram-a-mem.json`、`$HOME/.config/ram-a/ram-a-mem.json`、
  `/etc/ram-a/ram-a-mem.json`。
- 代码默认监听端口是 `8080`；RPM/本项目交付示例推荐使用 `18081`。两者不能混写为同一种
  默认值。

顶层字段如下：

| 字段 | 类别 | 省略时 | 说明 |
| --- | --- | --- | --- |
| `auth` | 部署必配 | 不允许省略 | Bearer Token 与主体映射 |
| `features` | 推荐可配置 | 使用各功能默认值 | 对外能力开关 |
| `http` | 推荐可配置 | 使用本地监听默认值 | HTTP 监听和来源限制 |
| `limits` | 推荐可配置 | 使用服务保护默认值 | 请求、并发和 Session 限制 |
| `pipeline` | 推荐可配置 | `fail_fast=true`、`max_memory_chars=500` | 记忆管线策略 |
| `storage` | 部署必配 | 生产校验失败 | 个人记忆 SQLite 文件 |
| `providers` | 部署必配 | 生产校验失败 | Extract、Ground 和 Embedding Provider |
| `retrieval` | 推荐可配置 | Hybrid，不启用 Rerank | 个人记忆检索策略 |
| `case_library` | 条件配置 | `null` | 案例库配置 |
| `graph_memory` | 条件配置 | `null` | 图记忆配置 |

## 3. `auth`

`auth.tokens[]` 每项创建一个 `Principal=(tenant_id,user_id,agent_id,permissions)`。
`scope_id` 固定由 `tenant_id + user_id` 派生；修改任一字段会切换数据隔离范围。

| 字段 | 类别 | 默认/推荐 | 约束与生效条件 | 测试要求 |
| --- | --- | --- | --- | --- |
| `tokens` | 部署必配 | 至少配置一个 | 非空数组 | 空数组启动失败；多 Token 可启动 |
| `tokens[].token_env` | 部署必配 | 使用能说明客户端的环境变量名，例如 `RAM_A_XIAOO_TOKEN` | 非空、无前后空格；数组内唯一；环境变量必须存在且值非空 | 缺失、空值、重复环境名及重复实际 Token 均失败；配置和日志不得出现密钥值 |
| `tokens[].tenant_id` | 部署必配 | 使用稳定租户标识 | 非空、无前后空格；参与 `scope_id` | 相同/不同租户的 scope 隔离 |
| `tokens[].user_id` | 部署必配 | 使用稳定用户标识 | 非空、无前后空格；参与 `scope_id` | 相同/不同用户的 scope 隔离 |
| `tokens[].agent_id` | 部署必配 | 使用调用 Agent 的稳定标识 | 非空、无前后空格；`x-agent-id` 提供时必须一致 | Header 缺省、匹配和不匹配 |
| `tokens[].permissions` | 固定值集合 | 按最小权限配置 | 元素只能是 `memory:read`、`memory:write`、`cases:read`、`cases:write`，同一 Token 内不得重复；空数组表示只能认证、不能调用受保护工具 | 四种合法值、未知值、重复值和权限不足 |

Token 本身没有由 RAM-A 管理的过期时间。轮换方式是更新环境变量或增加新 Token 配置并重启
服务；生命周期由部署系统负责。

## 4. `features`

| 字段 | 类别 | 代码默认 | 推荐值 | 约束与生效条件 |
| --- | --- | ---: | ---: | --- |
| `memory.enabled` | 推荐可配置 | `true` | `true` | 控制 `memory_ingest`、`memory_search`；图记忆开启时必须为 `true` |
| `case_library.enabled` | 条件配置 | `null` | 明确写 `true` 或 `false` | `null` 时由顶层 `case_library` 是否存在决定；`true` 时必须提供 `case_library` |
| `graph_memory.enabled` | 条件配置 | `false` | `false` | `true` 时必须开启 memory 并提供 `graph_memory` |

测试必须验证工具列表和调用行为随开关变化，而不只验证 JSON 能解析。

## 5. `http`

| 字段 | 类别 | 代码默认 | 推荐值 | 约束与生效条件 |
| --- | --- | --- | --- | --- |
| `bind_address` | 推荐可配置 | `127.0.0.1` | 单机/反向代理部署使用 `127.0.0.1` | IP 地址；非 loopback 时触发 TLS 和 Host 额外校验 |
| `port` | 推荐可配置 | `8080` | RPM/交付环境使用 `18081` | `u16`；应避免 `0` 和已占用端口，端口可绑定性由启动测试验证 |
| `allowed_origins` | 部署可配置 | `[]` | 无浏览器跨域需求时保持 `[]` | 精确匹配允许的 Origin；不影响非浏览器客户端 |
| `allowed_hosts` | 部署必配 | `localhost`、`127.0.0.1`、`::1` | 写实际访问 Host，包含端口时也应与请求一致 | 非空数组，元素非空；外部监听至少包含一个非 loopback Host |
| `tls_termination_acknowledged` | 条件配置 | `false` | loopback 为 `false`；外部监听且已有 TLS termination 时为 `true` | 只表示部署者确认，不会让 RAM-A 自己启用 TLS |

测试应覆盖本地默认配置、外部监听未确认 TLS、仅 loopback Host、外部 Host 正确配置四种情况。

## 6. `limits`

以下代码默认值同时作为推荐值，配置时不能超过固定支持范围。

| 字段 | 推荐值 | 固定有效范围 | 作用 |
| --- | ---: | ---: | --- |
| `max_body_bytes` | 16777216 | 1..=67108864 | 单个 MCP HTTP 请求体字节上限 |
| `requests_per_second` | 20 | 1..=10000 | 每 Principal、每 Tool 持续速率 |
| `rate_burst` | 40 | 1..=100000 | 每 Principal、每 Tool 突发容量 |
| `max_in_flight_per_principal_tool` | 4 | 1..=1024 | 每 Principal、每 Tool 并发上限；超限不排队 |
| `initialize_requests_per_second` | 4 | 1..=1000 | MCP initialize 持续速率 |
| `initialize_rate_burst` | 8 | 1..=10000 | MCP initialize 突发容量 |
| `max_active_sessions_per_principal` | 8 | 1..=1024 | 单 Principal 活动 MCP Session 上限 |
| `max_active_sessions_global` | 256 | 1..=100000 | 单进程活动 MCP Session 总上限 |
| `session_idle_timeout_seconds` | 1800 | 1..=86400 | RAM-A Admission 层的 Session 空闲回收时间 |

`max_active_sessions_global` 固定不得小于 `max_active_sessions_per_principal`。每个字段都必须有
默认值、下边界、上边界、0、上边界加一的配置测试；并发、速率、请求体和 Session 回收还应有
HTTP 行为测试。该 Session 超时不承诺修改底层 `rmcp` 自身的内部超时。

## 7. `pipeline`

| 字段 | 类别 | 代码默认/推荐 | 固定范围 | 生效条件与结果 |
| --- | --- | --- | --- | --- |
| `fail_fast` | 推荐可配置 | `true` | `true`/`false` | 只控制 Extract/Ground 窗口错误；`true` 终止本次摄入，`false` 跳过失败窗口并继续 |
| `max_memory_chars` | 推荐可配置 | `500` | 1..=32000 Unicode 字符 | 抽取记忆超限进入 quarantine，不截断 |

Episode 和 Window 的内部参数当前不是服务配置字段，不能写入 JSON。测试必须覆盖两种
`fail_fast` 行为，以及 `max_memory_chars` 的 1、500、32000、0、32001。

## 8. `storage`

| 字段 | 类别 | 默认/推荐 | 约束 | 测试要求 |
| --- | --- | --- | --- | --- |
| `database_path` | 部署必配 | RPM 推荐 `/var/lib/ram-a/ram-a-memory.sqlite` | 非空持久化 SQLite 文件路径；生产不接受 `:memory:`；不得与 `case_library.index_store` 相同 | 使用推荐绝对路径时服务启动成功、`/ready` 返回 2xx，并生成 SQLite/WAL 文件；空路径和 `:memory:` 必须启动失败；父目录只读或运行用户无写权限时必须启动失败并输出明确日志 |

`data/ram-a-memory.sqlite` 适合源码目录运行，不作为 RPM 推荐值。相对路径按进程工作目录解析；
RPM 验收应使用绝对路径，避免启动方式改变实际落盘位置。磁盘满、只读文件系统和 SQLite 锁冲突
应使用已安装的 RPM 二进制做存储可靠性故障注入，并检查进程状态、MCP 错误和脱敏日志。

## 9. `providers`

Extract 和 Ground 固定使用 OpenAI-compatible Chat API。Embedding 可以使用
OpenAI-compatible API 或本地确定性 hash。

| 字段 | 类别 | 代码默认 | 推荐值 | 约束与回退 |
| --- | --- | --- | --- | --- |
| `api_key_env` | 部署必配 | 无 | Chat Provider 的密钥环境变量名 | 非空且环境变量必须存在；即使 Embedding 使用 hash 也必需 |
| `base_url` | 部署可配置 | `https://openrouter.ai/api/v1` | 使用实际 Chat Provider 的 OpenAI-compatible `/v1` 基址 | 绝对 HTTP(S) URL；带密钥的公网地址必须 HTTPS |
| `embedding_provider` | 固定值枚举 | `openai_compatible` | 生产推荐 `openai_compatible`；离线自测可用 `hash` | 只接受 `openai_compatible`、兼容别名 `open_router`、`hash` |
| `embedding_api_key_env` | 条件配置 | `null` | 独立 Embedding 服务才填写 | `null` 回退到 `api_key_env`；配置时非空 |
| `embedding_base_url` | 条件配置 | `null` | 独立 Embedding 服务才填写 | `null` 回退到 `base_url`；URL 规则同上 |
| `embedding_model` | 部署必配 | 无 | `hash` Provider 写 `hash`；外部 Provider 写实际模型名 | 非空 |
| `embedding_dimensions` | 部署必配 | 无 | hash 自测推荐 1024；外部 Provider 必须使用模型实际输出维度 | 大于 0；已存向量与查询向量维度必须一致 |
| `extractor_model` | 部署必配 | 无 | 使用已验证能稳定输出 JSON 的 Chat 模型 | 非空；模型是否存在由联通测试验证 |
| `verifier_model` | 部署必配 | 无 | 推荐与 `extractor_model` 使用相同模型 | 非空；用于 Ground，也支持独立配置 |
| `timeout_seconds` | 推荐可配置 | 120 | 120 | 大于 0；Chat 请求超时 |
| `max_retries` | 推荐可配置 | 3 | 3 | 大于 0；仅作用于 Extract/Ground 共用的 Chat 客户端 |

配置测试验证枚举、默认值、非空、URL 安全规则和正数约束。模型存在性、API Key 权限、余额、
限流、响应 JSON 质量必须由带真实 Provider 的集成测试验证。

## 10. `retrieval`

| 字段 | 类别 | 代码默认 | 推荐值 | 约束与生效条件 |
| --- | --- | --- | --- | --- |
| `mode` | 固定值枚举 | `hybrid` | `hybrid` | MCP 服务只接受 `dense`、`bm25`、`hybrid`；独立 `graph` mode 启动失败 |
| `embedding_weight` | 推荐可配置 | 0.7 | 0.7 | Hybrid 时为有限数且 0..=1 |
| `bm25_weight` | 推荐可配置 | 0.3 | 0.3 | Hybrid 时为有限数且 0..=1；两权重之和固定为 1 |
| `candidate_k` | 推荐可配置 | `null` | 100 | `null` 使用 `max(top_k*5,100)`；显式值为 1..=500 |
| `rerank` | 条件配置 | 见下表 | 关闭 | 仅 Hybrid 可启用 |

Hybrid 推荐使用 0.7/0.3 权重组合。Dense 或 BM25 单通道模式不使用 Hybrid 权重。

### 10.1 `retrieval.rerank`

| 字段 | 类别 | 代码默认/推荐 | 约束与生效条件 |
| --- | --- | --- | --- |
| `enabled` | 条件配置 | `false` | `true` 时要求 `retrieval.mode=hybrid` 并构建 Rerank 客户端 |
| `provider` | 固定值 | `openrouter` | 当前只接受 `openrouter`；可指向兼容该请求协议的自托管端点 |
| `model` | 部署可配置 | `cohere/rerank-v3.5` | 启用时非空；需与端点实际模型一致 |
| `api_key_env` | 条件配置 | `OPENROUTER_API_KEY` | 可设 `null` 以访问无需认证的可信本地端点；非空时环境变量必须存在 |
| `base_url` | 部署可配置 | `https://openrouter.ai/api/v1` | 启用时必须是合法 URL；公网带密钥固定要求 HTTPS |
| `input_k` | 推荐可配置 | 40 | 1..=500；运行时实际送入数至少为请求 `top_k` |
| `timeout_ms` | 推荐可配置 | 30000 | 启用时必须为 1..=120000；禁用时不生效 |
| `fail_open` | 推荐可配置 | `false` | `false`：Rerank 异常使本次 search 返回 `RERANK_FAILED`；`true`：返回 Rerank 前 Hybrid 顺序 |

测试必须覆盖启用/禁用、非 Hybrid 启用失败、input/timeout 边界、无认证本地端点、公网 HTTP
拒绝，以及 `fail_open` 两种故障结果。排序效果和稳定性不能只靠配置测试证明。

## 11. `case_library`

只有案例库功能生效时，下列 Provider、导入 Worker 和 REST 管理接口才会构建。

| 字段 | 类别 | 代码默认 | 推荐值 | 约束与生效条件 |
| --- | --- | --- | --- | --- |
| `rag_store` | 部署可配置 | `data/memory-cases.sqlite` | 使用独立持久化文件 | 非空、非 `:memory:`，且不同于 `index_store` |
| `index_store` | 部署可配置 | `data/memory-cases-index.sqlite` | 使用独立持久化文件 | 非空、非 `:memory:`，且不同于 `rag_store` 和个人记忆库 |
| `source_dir` | 条件配置 | `null` | 仅需启动时自动扫描本地案例时填写 | 配置时路径非空；目录可读性由集成测试验证 |
| `api_token_env` | 条件配置 | `null` | 不使用案例管理 REST API 时保持 `null` | 配置后启用管理 API；环境变量名及值必须非空、无前后空格 |
| `ingestion_poll_ms` | 推荐可配置 | 1000 | 1000 | 大于 0；案例摄入 Worker 轮询间隔 |
| `embedding_provider` | 固定值枚举 | `openai_compatible` | 生产明确配置 `openai_compatible`；离线自测明确配置 `hash` | 枚举同 `providers.embedding_provider`；不要依赖默认 Provider 与默认模型的组合 |
| `embedding_api_key_env` | 条件配置 | `null` | 独立案例 Embedding 服务才填写 | `null` 回退到 `providers.api_key_env` |
| `embedding_base_url` | 条件配置 | `null` | 独立案例 Embedding 服务才填写 | `null` 回退到 `providers.base_url` |
| `embedding_model` | 推荐可配置 | `hash` | hash 自测用 `hash`；生产写实际模型 | 非空 |
| `embedding_dimensions` | 推荐可配置 | 1024 | hash 自测 1024；生产与实际模型一致 | 大于 0 |
| `chunk_size` | 推荐可配置 | 160 | 160 | 大于 0；案例切块大小 |
| `summary_llm_model` | 条件配置 | `null` | 不需要模型摘要时保持 `null` | 非 `null` 时启用摘要模型 |
| `summary_llm_api_key_env` | 条件配置 | `null` | 摘要服务使用独立密钥时填写 | `null` 回退到 `providers.api_key_env` |
| `summary_llm_base_url` | 条件配置 | `null` | 摘要服务使用独立端点时填写 | `null` 回退到 `providers.base_url` |
| `summary_llm_timeout_ms` | 推荐可配置 | 30000 | 30000 | 大于 0；只在摘要模型启用时实际调用 |
| `default_library` | 部署必配 | 无 | 选择最常用逻辑库 | 非空，必须引用 `libraries[].name` |
| `libraries` | 部署必配 | 无 | 至少一项 | 非空；`name` 唯一 |
| `libraries[].name` | 部署必配 | 无 | 使用稳定、面向调用方的逻辑库名 | 非空、无前后空格、数组内唯一 |
| `libraries[].dataset_id` | 部署必配 | 无 | 使用内部稳定 dataset ID | 非空、无前后空格 |
| `libraries[].tenant_ids` | 部署必配 | 无 | 明确列出允许访问的租户 | 非空数组，元素非空且无前后空格 |

配置测试覆盖默认值、持久化路径隔离、正数约束和映射关系。目录内容、文档导入、Embedding
维度及摘要模型输出属于案例库集成测试。

## 12. `graph_memory`

| 字段 | 类别 | 代码默认 | 推荐值 | 约束与生效条件 |
| --- | --- | --- | --- | --- |
| `llm_api_key_env` | 部署必配 | 无 | 图模型密钥环境变量名 | 非空；功能开启时环境变量必须存在 |
| `llm_base_url` | 部署可配置 | `https://openrouter.ai/api/v1` | 使用实际 OpenAI-compatible 图模型端点 | URL 安全规则同主 Provider |
| `llm_model` | 部署必配 | 无 | 使用支持图 schema 输出的模型 | 非空 |
| `llm_timeout_ms` | 推荐可配置 | 60000 | 60000 | 大于 0 |
| `build_concurrency` | 推荐可配置 | 1 | 1 | 大于 0；一次摄入内图构建并发数 |
| `retrieval.weight` | 推荐可配置 | 0.2 | 0.2 | 有限数，0..=1 |
| `retrieval.rerank_with_graph` | 推荐可配置 | `false` | `false` | 决定图结果是否进入 Rerank 输入 |
| `retrieval.allow_graph_only` | 推荐可配置 | `false` | `false` | 是否允许无核心记忆支撑的纯图结果 |
| `retrieval.max_graph_only_results` | 条件配置 | `null` | `null` | 显式配置时大于 0；限制纯图结果数 |
| `retrieval.seed_limit` | 条件配置 | `null` | `null` | 显式配置时 1..=5000；`null` 使用核心动态规则 |
| `retrieval.max_evidence_records_per_fact` | 条件配置 | `null` | `null` | 显式配置时 1..=100 |
| `retrieval.fail_open` | 推荐可配置 | `false` | 图是增强通道且要求核心检索可用时推荐 `true` | `true` 时图检索失败退回非图结果；不控制图摄入失败 |

测试覆盖功能依赖、默认值、weight、三个可选容量字段及 `fail_open`。图模型输出质量、图构建
效果和融合排序需要真实模型或确定性桩测试。

## 13. 非 JSON 启动配置

这些值在日志或配置文件加载之前生效，因此不放入 `ram-a-mem.json`。

| 环境变量 | 类别 | 默认/推荐 | 固定值或约束 |
| --- | --- | --- | --- |
| `RAM_A_MEM_CONFIG` | 部署可配置 | 未使用 `--config` 时指向交付配置 | 文件路径；优先级低于 CLI `--config` |
| `RAM_A_LOG_FORMAT` | 固定值枚举 | 默认 `json`；终端调试推荐 `compact` | 只接受 `json`、`compact`，非法值启动失败 |
| `RAM_A_LOG_SOURCE` | 固定值枚举 | 默认 `false`；现场定位推荐临时设 `true` | 只接受 `true`、`false`，非法值启动失败 |
| `RUST_LOG` | 部署可配置 | 未设置时使用服务默认过滤级别 | tracing filter 表达式；不改变业务结果 |
| 各 `*_env` 指向的变量 | 部署必配/条件配置 | 无 | 保存实际密钥；不得写入 JSON、日志或测试快照 |

## 14. RPM 验收与源码自动化边界

配置验收以 RPM 安装后的真实二进制为对象。每个字段用例按以下方式执行：

1. 从推荐配置生成一个字段变体，注入有效 Token 和 Provider 环境变量。
2. 启动 RPM 提供的 `ram-a-mem` 二进制；合法配置要求进程存活且 `/healthy`、`/ready` 返回 2xx。
3. 对功能开关、限流、Pipeline、检索和权限字段发送 MCP 请求，验证外部行为而不只检查启动结果。
4. 对非法值要求进程启动失败或请求返回规定错误，并检查日志包含字段或错误分类且不泄露密钥。
5. 保存实际配置、RPM 版本、进程退出码、HTTP/MCP 响应和日志作为验收证据。

源码自动化应至少覆盖：

1. 完整示例可以反序列化、通过运行时校验，并与 `ServerConfig` 全字段序列化结果一致。
2. 所有有代码默认值的字段在省略后得到文档值。
3. 所有固定枚举接受全部合法值并拒绝未知值。
4. 所有具有固定数值范围的字段接受上下边界并拒绝 0、越界值和不一致组合。
5. 所有条件配置覆盖关闭、正确开启、依赖缺失和回退字段。
6. 所有部署字符串和路径覆盖非空、规范化、URL 安全、引用关系或文件隔离，不增加无依据长度上限。

当前源码自动化已覆盖上述六项：

| 条目 | 主要自动化用例 |
| --- | --- |
| 完整示例与全字段 Schema | `packaged_rpm_example_matches_server_schema` |
| 全部代码默认值 | `feature_http_and_provider_defaults_are_stable`、`graph_and_case_library_defaults_are_stable`、`http_limit_defaults_are_stable_and_supported`、`pipeline_defaults_and_boundaries_are_stable`、`retrieval_defaults_preserve_current_hybrid_behavior` |
| 固定枚举 | `configurable_enums_reject_unsupported_values`、`authentication_configuration_enforces_fixed_permissions_and_canonical_ids` |
| 数值范围和组合约束 | `http_limits_accept_documented_lower_boundaries`、`http_limits_accept_documented_upper_boundaries`、`http_limits_reject_zero_out_of_range_and_inconsistent_sessions`、`graph_configuration_accepts_all_documented_boundaries`、`graph_configuration_rejects_invalid_configurable_values`、`retrieval_accepts_hybrid_weight_boundaries`、`retrieval_candidate_and_rerank_limits_accept_boundaries`、`retrieval_rejects_candidate_and_rerank_values_outside_limits` |
| 条件配置与回退 | `provider_and_case_library_fallbacks_are_explicit_and_overridable`、`disabled_rerank_ignores_inactive_provider_fields`、`enabled_rerank_rejects_every_invalid_provider_field`，以及 memory-core 的 Rerank/Graph `fail_open` 用例 |
| 字符串、URL 和路径 | `provider_configuration_rejects_incomplete_configurable_values`、`provider_base_url_rejects_credentials_query_and_fragment`、`case_library_paths_and_mappings_reject_every_invalid_shape`、`storage_configuration_rejects_nonpersistent_paths_and_accepts_file_paths`、`http_configuration_covers_host_and_port_boundaries` |

配置单元测试不能证明以下内容，必须由 RPM 容器或目标环境验收：端口和目录权限、Provider 网络可达、
API Key 权限/余额、模型名存在、模型 JSON 输出质量、Embedding 实际维度、Rerank 排序效果、
SQLite 磁盘满/只读/锁冲突、吞吐容量和 TLS termination 是否真实部署。

源码中的 `CaseServiceConfig` 是独立案例服务客户端的内部配置类型，不是 `ServerConfig` 顶层
字段；`base_url`、`bearer_token_env` 等字段不能写入 `ram-a-mem.json`。同样，xiaoO 的
`memory_automation` 和 MCP Server 注册项属于 Agent 配置，不属于 RAM-A 服务配置。

配置相关源码测试命令：

```bash
cargo test -p memory-mcp --lib config::tests
cargo test -p memory-mcp --test config_auth
cargo test -p memory-mcp --test http_mcp
```

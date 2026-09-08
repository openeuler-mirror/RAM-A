# RAM-A-MEM 日志与错误可观测性设计

状态：核心日志格式与错误分类已实现，剩余验收项见第 11 节

本文定义 RAM-A-MEM 的日志输出、请求关联、错误分类和脱敏规则，并记录当前工作树的
实现及自动化覆盖。未标注“已覆盖”的验收项仍不能作为已验证能力使用。

## 1. 背景与现状

当前日志和错误处理存在以下问题：

1. `init_tracing()` 固定输出 JSON，不能在日志平台采集和终端排障之间切换。
2. 终端直接查看 JSON 日志时单条记录过长，不利于使用 `tail`、`journalctl` 等工具。
3. 日志默认不包含源码文件名和行号，现场排障后仍需二次搜索代码。
4. MCP 工具错误响应未返回 `request_id`，调用方难以将失败响应与服务端日志关联。
5. Pipeline 错误已经保留 `stage`，但根因在映射为 `PIPELINE_FAILED` 时仍会丢失；
   embedding、幂等表、SQLite busy/readonly 与记忆存储错误在代码中已区分为
   `EMBEDDING_FAILED`、`IDEMPOTENCY_STORAGE_FAILED`、`SQLITE_BUSY`、
   `SQLITE_READONLY`、`VECTOR_PERSIST_FAILED` 等具体错误码，`STORAGE_FAILED`
   仅作为无法归类的兜底。
6. Extract 处理多消息或复杂窗口失败时，当前信息不足以区分网络错误、空模型输出、
   Extract JSON 非法、Extract schema 不匹配等情况；Provider 日志中的
   `error_kind=request` 仍然过粗。

当前未提交代码已经形成以下基线，本设计在此基础上增量完善：

| 能力 | 当前状态 |
| --- | --- |
| JSON 结构化输出 | 已支持，可通过环境变量选择。 |
| 七阶段 started/completed/failed 事件 | 已支持，并有 `pipeline_logging.rs` 自动化用例。 |
| Pipeline 失败阶段透传 | 已支持，MCP 错误可返回 `stage=extract/ground` 等值。 |
| LLM/Rerank retry 和 failed 事件 | 已支持；LLM 可区分空内容、非法 JSON、响应读取、超时、HTTP 状态和连接错误。 |
| Rerank 独立错误 | 已支持 `RERANK_FAILED`。 |
| request span | 已支持，日志可继承 `request_id` 和 `scope_id_hash`。 |
| Compact、源码位置、错误响应 request_id | 已支持；格式器、非法配置和 MCP 失败响应均有自动化用例。 |
| 细粒度 Extract/Storage 根因 | 已支持主要类别；未知存储错误仍保留 `STORAGE_FAILED` 兜底。 |

## 2. 目标与非目标

### 2.1 目标

- 通过环境变量在 `json` 和 `compact` 两种格式之间切换。
- 可选择输出源码文件名和行号。
- 使用稳定业务字段关联 HTTP 请求、MCP 工具调用和 Pipeline 执行。
- 在不记录用户内容和凭据的前提下，区分主要失败组件和失败类型。
- 为格式、字段、错误映射、脱敏和行为不变性提供自动化测试。

### 2.2 非目标

- 不在 RAM-A-MEM 进程内实现日志文件落盘、轮转、压缩和保留。
- 不把源码文件名、行号或自然语言错误摘要定义为稳定告警接口。
- 不记录模型原始请求、模型原始响应、消息正文或记忆正文来辅助排障。
- 本设计不引入分布式追踪系统；`request_id` 和 `pipeline_run_id` 是本阶段的关联手段。
- 本设计不改变记忆抽取、校验、Grounding、持久化或检索算法。

## 3. 配置设计

日志渲染配置只从环境变量读取。目标状态下，日志级别由 `RUST_LOG` 控制，未设置时
默认为 `info`。现有版本若已提供 `logging.level`，应在迁移时标记为弃用；不能为了
读取该字段而把日志初始化推迟到完整服务配置加载之后。

| 环境变量 | 合法值 | 默认值 | 作用 |
| --- | --- | --- | --- |
| `RAM_A_LOG_FORMAT` | `json`、`compact` | `json` | 选择结构化采集格式或终端紧凑格式。 |
| `RAM_A_LOG_SOURCE` | `true`、`false` | `false` | 是否为全部日志增加事件打印位置；不控制失败日志必备的错误起点。 |

解析规则：

1. 环境变量未设置时使用默认值。
2. 值按区分大小写的精确字符串解析；空字符串、前后空格、`JSON`、`1`、`yes` 等
   均视为非法值。
3. 任一值非法时，进程必须在创建 HTTP listener、打开数据库和构造外部 Provider
   之前退出，退出码非 0。
4. 由于此时日志系统尚未初始化，启动错误直接向 stderr 输出单行安全文本，文本包含
   环境变量名、非法值类别和合法值列表，但不得打印其他环境变量。

示例：

```text
invalid RAM_A_LOG_FORMAT; expected one of: json, compact
```

建议新增独立的 `LogSettings::from_env()`，并在 `main` 的业务配置加载和运行时资源
初始化之前完成解析与 subscriber 初始化。格式和源码位置选项不加入
`ram-a-mem.json`，避免服务配置文件和环境变量形成两套优先级。

## 4. 输出目标

RAM-A-MEM 应将应用日志写入 stderr，由运行环境负责收集：

- systemd：journald；
- 容器：容器日志驱动；
- 文件部署：由 shell 重定向或日志采集代理落盘；
- 文件轮转和保留：由 journald、容器运行时或 logrotate 管理。

目标实现不直接创建 `/var/log/ram-a`，也不持有日志文件句柄，不新增
`logging.directory` 等进程内文件输出配置。

## 5. 日志格式

### 5.1 JSON 格式

`RAM_A_LOG_FORMAT=json` 时，每条日志为一个完整 JSON 对象，并以换行分隔。RAM-A
自身产生的业务日志至少满足以下结构：

```json
{
  "timestamp": "2026-08-20T08:30:00.123456Z",
  "level": "ERROR",
  "target": "memory_mcp::service",
  "fields": {
    "event": "ram_a.memory.ingest.failed",
    "message": "memory ingest failed",
    "operation": "memory_ingest",
    "request_id": "4f65c9ad-7db2-4dbe-a836-3bce6adbd736",
    "pipeline_run_id": "run-b4a31b0c-2c5d-47e3-b895-8df12e90323a",
    "stage": "vector_persist",
    "error_code": "EMBEDDING_FAILED",
    "source_error_kind": "timeout",
    "source_error_message": "embedding provider timed out",
    "error_site": "memory_mcp.ingest.vector_persist",
    "error_origin_file": "crates/memory-core/src/embedding.rs",
    "error_origin_line": 153
  },
  "filename": "crates/memory-mcp/src/service.rs",
  "line_number": 298
}
```

字段要求：

| 字段 | 要求 |
| --- | --- |
| `timestamp` | 必须存在，使用 UTC RFC3339 时间。 |
| `level` | 必须存在，取 `TRACE/DEBUG/INFO/WARN/ERROR`。 |
| `target` | 必须存在，用于区分模块；不得关闭 target 输出。 |
| `fields.event` | RAM-A 自有业务事件必须存在，使用稳定事件名。 |
| `fields.message` | RAM-A 自有事件必须存在，提供简短、可读且不含用户内容的说明。 |
| `fields.operation` | 工具、Pipeline、Provider 和存储事件必须存在，例如 `memory_ingest`、`chat_completion`。 |
| `fields.request_id` | 进入 HTTP middleware 后产生的日志必须尽可能携带。 |
| `fields.pipeline_run_id` | 已创建 Pipeline run 后的摄入日志必须携带。 |
| `fields.stage` | Pipeline 阶段和服务内部阶段事件必须携带。 |
| `fields.error_code` | 对外或内部失败事件必须携带。 |
| `fields.error_site` | RAM-A 自有失败事件必须存在，标识稳定的逻辑错误点。 |
| `filename`、`line_number` | 日志打印位置，仅在 `RAM_A_LOG_SOURCE=true` 时存在。 |
| `fields.error_origin_file`、`fields.error_origin_line` | 错误首次在 RAM-A 中形成或被分类的位置；RAM-A 自有失败日志必须存在，不受 source 开关影响。 |

`rmcp`、`hyper` 等第三方 crate 的日志可能没有 `fields.event`。该要求只约束 RAM-A
自有事件；告警规则应通过 `target` 或 `fields.event` 过滤掉不相关的第三方日志。

### 5.2 Compact 格式

`RAM_A_LOG_FORMAT=compact` 面向 `tail`、`journalctl` 和容器手工测试。它不是简单地
把 JSON 字段全部摊平成一行，而是使用固定视觉顺序，将最重要的信息放在前面。

普通事件和失败事件分别使用以下固定格式：

```text
普通事件：[<timestamp>] [<level>] [<target>] [<operation>/<stage>] <message> | <context fields>
失败事件：[<timestamp>] [<level>] [<error origin>] [<operation>/<stage>] <error_code>: <error summary> | <diagnostic fields>
```

失败事件把真正的 RAM-A 错误起点放在视觉前部，不要求排障人员先扫到行尾。字段顺序
固定为：

1. `timestamp`：UTC RFC3339，精确到毫秒。
2. `level`：`TRACE/DEBUG/INFO/WARN/ERROR`。
3. 普通事件显示 `target`；失败事件显示 `error_origin_file:error_origin_line`。
4. `operation/stage`：例如 `memory_ingest/extract`；没有 stage 时只显示 operation。
5. 正常事件使用安全的 `message`；失败事件使用
   `<error_code>: <source_error_message>`。
6. 后置字段：失败事件先输出 `target` 和 `error_site`，随后按约定顺序输出 request_id、
   pipeline_run_id、retriable、进度和耗时等 `key=value` 字段。

失败示例：

```text
[2026-08-20T08:30:00.123Z] [ERROR] [crates/memory-core/src/embedding.rs:153] [memory_ingest/vector_persist] EMBEDDING_FAILED: embedding provider timed out | target=memory_mcp::service site=memory_mcp.ingest.vector_persist request_id=4f65c9ad-7db2-4dbe-a836-3bce6adbd736 pipeline_run_id=run-b4a31b0c-2c5d-47e3-b895-8df12e90323a retriable=true
```

阶段成功示例：

```text
[2026-08-20T08:30:00.456Z] [INFO] [memory_pipeline::pipeline] [memory_ingest/extract] extraction completed | request_id=4f65c9ad-7db2-4dbe-a836-3bce6adbd736 pipeline_run_id=run-b4a31b0c-2c5d-47e3-b895-8df12e90323a completed_units=1 total_units=1 elapsed_ms=328
```

Provider 重试示例：

```text
[2026-08-20T08:30:01.000Z] [WARN] [memory_pipeline::client] [provider/chat_completion] retrying LLM request after timeout | request_id=4f65c9ad-7db2-4dbe-a836-3bce6adbd736 model=glm-5.1 attempt=2 max_attempts=3 backoff_ms=2000
```

渲染规则：

- 每条事件严格单行；换行、回车和制表符转义为 `\n`、`\r`、`\t`。
- 字符串含空格、引号或 `=` 时使用 JSON 字符串转义，避免字段边界不清。
- 已知诊断字段按上面的顺序输出，其余字段按字段名排序，保证同类日志布局稳定。
- 不重复输出已经进入固定前缀的 `timestamp`、`level`、`target`、operation 和 stage。
- 失败日志始终包含 origin；source=true 时额外增加
  `log_at=<filename>:<line_number>`，用于定位日志打印行。
- 不输出 ANSI 颜色码到非终端输出；是否在交互式 TTY 中按级别着色属于实现细节，
  不能影响文本内容和自动化断言。

Compact 格式只改变渲染方式，不改变底层结构化事件字段。实现上应提供 RAM-A 自定义
`FormatEvent`，不能直接依赖 tracing 默认 Compact 的字段顺序。

这一设计采用主流日志系统的共同结构：OpenTelemetry 将 timestamp、severity、可读
Body、EventName 和 Attributes 分开；Go `slog` 的文本格式固定输出 time、level、msg
后再跟结构化属性；Log4j Pattern Layout 使用日期、级别、logger、message 的固定模式；
systemd `journalctl short-iso` 使用 RFC3339 单行展示。参考：

- [OpenTelemetry Logs Data Model](https://opentelemetry.io/docs/specs/otel/logs/data-model/)
- [Go structured logging with slog](https://go.dev/blog/slog)
- [Apache Log4j Pattern Layout](https://logging.apache.org/log4j/2.x/manual/pattern-layout.html)
- [tracing-subscriber fmt formatters](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/fmt/index.html)
- [journalctl output formats](https://www.freedesktop.org/software/systemd/man/latest/journalctl.html)

### 5.3 源码位置

单独启用 tracing subscriber 的源码字段还不足以定位根因。它只说明 `error!()` 在哪一
行执行；如果日志在 `service.rs` 的 `map_err` 中产生，只能定位到错误映射代码，不能
定位到 `embedding.rs`、Extractor JSON 解析或 SQLite 操作的实际失败边界。

因此错误起点不能依赖 `RAM_A_LOG_SOURCE`。两类位置的输出条件如下：

| 字段 | 含义 |
| --- | --- |
| `filename`、`line_number` | 日志事件的打印位置，由 tracing subscriber 提供；source=true 时为全部日志输出。 |
| `error_origin_file`、`error_origin_line` | 错误首次在 RAM-A 代码中创建，或第三方错误首次被 RAM-A 分类的位置；RAM-A 自有失败日志始终输出。 |
| `error_site` | 不依赖物理行号的稳定逻辑位置，例如 `memory_pipeline.extract.parse_response`；RAM-A 自有失败日志始终输出。 |

具体行为：

| 日志类型 | source=false | source=true |
| --- | --- | --- |
| 成功、启动、阶段开始等普通日志 | 不输出物理源码位置。 | 输出 `filename/line_number`。 |
| RAM-A 自有失败日志 | 输出 `error_site` 和 `error_origin_file/error_origin_line`。 | 在左侧字段基础上，额外输出日志打印位置 `filename/line_number`。 |
| 第三方 crate 日志 | 不保证源码位置。 | 尽可能输出 `filename/line_number`；不要求有 RAM-A error origin。 |

错误起点遵循“第一次记录，后续只传递”的原则：

1. RAM-A 自身发现非法 JSON、schema 错误、维度不匹配等问题时，在创建结构化错误的
   同一行记录 origin。
2. reqwest、rusqlite 等第三方错误没有可依赖的 RAM-A 源码行；在它们第一次转换成
   RAM-A 错误的边界记录 origin，例如 embedding HTTP 调用、幂等 reserve SQL 或
   vector store 写入位置。
3. `PipelineError::at_stage`、`ServiceError` 映射和 MCP 响应包装必须保留已有 origin，
   不得用外层 `map_err` 行号覆盖。
4. Provider 服务端内部哪一行失败无法由 RAM-A 获得；日志只能精确到 RAM-A 的调用
   边界，并结合 HTTP 状态和 `source_error_kind` 判断远端原因。

建议错误类型携带以下上下文：

```rust
pub struct ErrorOrigin {
    pub site: &'static str,
    pub file: &'static str,
    pub line: u32,
}
```

通过带 `#[track_caller]` 的错误构造函数或统一宏读取
`std::panic::Location::caller()`。不建议默认打印 Backtrace：它体积大、依赖调试符号，
还可能暴露构建路径；Backtrace 可作为开发期临时诊断手段，但不属于稳定日志协议。

`filename/line_number` 和 `error_origin_file/error_origin_line` 都会随源码重构改变，不能
用于稳定告警。日志检索先使用 `error_site`、`event`、`stage` 和 `error_code`，需要查看
具体实现时再使用文件名和行号。默认 source=false 不影响失败日志定位，只减少普通日志
中的物理源码字段。

## 6. 关联标识与业务字段

### 6.1 标识传播

1. HTTP middleware 为每个请求生成 `request_id`，继续通过 `x-request-id` 响应头
   返回。
2. `request_id` 必须写入请求 extension/span，使 MCP handler、MemoryService、
   Pipeline、Provider 和持久化日志继承同一个值。
3. `memory_ingest` 在幂等 reserve 确定 run 后，将 `pipeline_run_id` 加入后续阶段日志。
4. MCP 工具错误的 `structuredContent` 返回 `request_id`，并与 HTTP 响应头一致。
5. 将 `pipeline_run_id` 同时返回到失败响应仍是后续项；当前调用方可先通过
   `request_id` 关联包含 `pipeline_run_id` 的服务端失败日志。

目标工具错误响应示例（当前实现尚不返回其中的 `pipeline_run_id`）：

```json
{
  "code": "PIPELINE_FAILED",
  "message": "memory pipeline failed",
  "retriable": true,
  "request_id": "4f65c9ad-7db2-4dbe-a836-3bce6adbd736",
  "pipeline_run_id": "run-b4a31b0c-2c5d-47e3-b895-8df12e90323a",
  "stage": "extract"
}
```

响应不返回 `source_error_message`，避免向客户端暴露内部实现、Provider 返回内容或
存储路径。调用方使用 `request_id` 查询服务端日志。

### 6.2 安全业务字段

日志可记录以下统计或派生字段：

- `scope_id_hash`、`conversation_id_hash`：带版本前缀的 SHA-256 摘要前 16 个小写
  十六进制字符，仅用于日志关联；
- `message_count`、`candidate_message_count`；
- `window_count`、`window_message_count`、`window_candidate_count`；
- `accepted_count`、`rejected_count`、`quarantined_count`；
- `record_count`、`completed_units`、`total_units`；
- Provider 类型、模型名、attempt、max_attempts、backoff_ms 和 HTTP 状态码。

不得记录原始 `scope_id`、`conversation_id`、`message_id` 或用未加版本前缀的裸
摘要代替上述 hash。日志 hash 只用于关联，不能参与鉴权、幂等或数据查询。

## 7. 事件模型

事件名使用小写点分层命名，字段值使用 `snake_case`。第一阶段至少覆盖下列事件：

| event | level | 必要业务字段 |
| --- | --- | --- |
| `ram_a.service.started` | INFO | `message`、`log_format`、`log_source` |
| `ram_a.http.request.completed` | INFO | `request_id`、`operation`、`status`、`duration_ms` |
| `ram_a.memory.ingest.started` | INFO | `request_id`、两个业务 hash、消息数量 |
| `ram_a.memory.ingest.stage.started` | INFO | `request_id`、`pipeline_run_id`、`stage`、进度字段 |
| `ram_a.memory.ingest.stage.completed` | INFO | 上述字段、`elapsed_ms` |
| `ram_a.memory.ingest.stage.window_skipped` | WARN | 当前窗口失败但 `fail_fast=false` 继续执行时，记录阶段、窗口 hash 和错误诊断字段 |
| `ram_a.memory.ingest.stage.failed` | ERROR | 上述字段、错误诊断字段 |
| `ram_a.memory.ingest.completed` | INFO | 两个关联 ID、accepted/rejected/quarantined 数量、`latency_ms` |
| `ram_a.memory.ingest.failed` | ERROR | 可用的关联 ID、`stage`、错误诊断字段、`latency_ms` |
| `ram_a.memory.search.started` | INFO | `request_id`、`scope_id_hash`、`query_hash`、`top_k`、检索模式 |
| `ram_a.memory.search.completed` | INFO | `request_id`、返回数量、检索模式、`latency_ms` |
| `ram_a.memory.search.failed` | ERROR | `request_id`、`stage`、错误诊断字段、`latency_ms` |
| `ram_a.provider.retry` | WARN | 关联 ID、`component`、Provider、model、attempt、backoff、错误分类 |
| `ram_a.provider.failed` | ERROR | 关联 ID、`component`、Provider、model、attempts、错误分类 |
| `ram_a.storage.operation.failed` | ERROR | 关联 ID、`component`、`storage_operation`、错误分类 |

七阶段 Pipeline 的 `stage` 固定为：

```text
normalize, episode, window, extract, validate, ground, aggregate
```

服务编排阶段可额外使用：

```text
request_validate, idempotency_reserve, vector_persist, idempotency_complete,
search_embedding, dense_recall, bm25_recall, hybrid_fuse, rerank, post_filter
```

这些 stage 值可用于日志检索和测试，但增加新阶段是允许的。重命名或改变已有阶段语义
需要同步更新文档和测试。

`stage.failed` 只表示该失败终止了本次 Pipeline。`fail_fast=false` 时，Extract 或
Verify 的单个窗口失败使用 `stage.window_skipped`，随后仍应产生后续阶段事件和最终
`ingest.completed`；不得同时将这次请求记录成 `ingest.failed`。

## 8. 错误模型

### 8.1 分层原则

错误分为四层：

1. 对外 `error_code`：供 MCP 调用方决定是否重试和如何提示。
2. `stage`、`component`、`storage_operation`：指出失败发生的位置。
3. 所有 RAM-A 自有失败日志中的 `error_site` 和 origin：定位到逻辑错误点和源码行。
4. 仅日志可见的 `source_error_kind`、`source_error_message`：说明底层失败类别。

服务内部错误类型必须同时保留 cause 和最初的 `ErrorOrigin`，不能在 `map_err` 时立即
压缩为无上下文的枚举值。建议将 `ServiceError` 改为携带 code、stage、retriable、
origin 和 source 的结构，或为每个错误变体保留 `#[source]` 和 origin。

### 8.2 对外错误码

| error_code | 适用范围 | retriable |
| --- | --- | --- |
| `INVALID_REQUEST` | 请求字段或业务边界校验失败。 | `false` |
| `IDEMPOTENCY_CONFLICT` | 同一幂等消息的 content hash 改变。 | `false` |
| `PIPELINE_FAILED` | Normalize 至 Aggregate 阶段失败；用 `stage` 区分。 | 由根因决定 |
| `RERANK_FAILED` | Rerank 已启用且调用失败，同时 `fail_open=false`。 | `true` |
| `EMBEDDING_FAILED` | 摄入或检索调用 embedding Provider 失败或返回无效向量。 | `true`（`map_memory_error` 对 embedding 网络失败按瞬态处理） |
| `IDEMPOTENCY_STORAGE_FAILED` | 幂等 reserve/complete 失败，且未命中更具体的 SQLite 错误。 | `true` |
| `SQLITE_BUSY` | 任意 SQLite 操作返回 busy/locked。 | `true` |
| `SQLITE_READONLY` | SQLite 数据库或目录不可写。 | `false` |
| `VECTOR_PERSIST_FAILED` | embedding 已完成，但记忆记录或向量写入失败，且未命中具体 SQLite 错误。兜底分支默认 `false`（磁盘满、只读文件系统等常见根因重试无益），命中 `SQLITE_BUSY` 等瞬态错误时按对应错误码返回 `true`。 | `false` |
| `STORAGE_FAILED` | 无法归入上述类别的兼容性兜底。 | `true` |

Provider 根因的 `retriable` 规则：连接失败、超时、HTTP 408/425/429/5xx 为 `true`；
其他明确的 HTTP 4xx、配置错误和 embedding 维度不匹配为 `false`。模型返回空内容在内部
重试耗尽后仍映射为 `PIPELINE_FAILED`（下一次生成可能不同，标记为 `true`）；而输入、
非法 JSON 和 schema 错误对同一请求体是永久性的，映射为 `PIPELINE_FAILED` 时标记为
`false`（`PipelineError::is_retriable` 按根因类别区分）。

错误码选择优先级如下：

```text
SQLITE_BUSY / SQLITE_READONLY
  > EMBEDDING_FAILED
  > IDEMPOTENCY_STORAGE_FAILED / VECTOR_PERSIST_FAILED
  > STORAGE_FAILED
```

例如，幂等 reserve 遇到 SQLite busy 时返回 `SQLITE_BUSY`，同时日志中的
`component=idempotency`、`storage_operation=reserve` 表明具体位置，不再同时返回
`IDEMPOTENCY_STORAGE_FAILED`。

### 8.3 内部错误分类

`source_error_kind` 使用受控枚举，不直接使用底层错误字符串：

```text
timeout, connect, http_status, response_read, empty_content, invalid_json,
schema_invalid, embedding_invalid_response, embedding_dimension_mismatch,
sqlite_busy, sqlite_readonly, sqlite_other, io, cancelled, internal
```

模型输出 schema 问题可额外记录不含原文的 `schema_issue`：

```text
root_not_object, schema_version_mismatch, memories_not_array,
memory_not_object, duplicate_memory_id, results_not_array,
missing_result, duplicate_result, unexpected_result, invalid_enum
```

`source_error_message` 必须由本地受控模板生成，而不是直接记录 HTTP body、模型输出或
任意 `Display` 链。它必须满足：

- 单行；
- 最多 512 个 Unicode 字符；
- 不包含 URL query、请求/响应 body、文件中的业务数据或环境变量值；
- 对 Authorization、Bearer、token、api_key 等模式执行二次脱敏；
- 可用于人工阅读，但不作为告警条件。

### 8.4 多消息 Extract 诊断

多消息 Extract 失败时，日志必须能够回答“哪个窗口、失败在哪一类”，但不能泄露窗口
内容。失败事件至少记录：

```text
request_id, pipeline_run_id, stage=extract, window_id_hash,
window_message_count, window_candidate_count, model, attempts,
error_code=PIPELINE_FAILED, source_error_kind
```

按失败情况补充：

| 情况 | source_error_kind | 可选字段 |
| --- | --- | --- |
| 连接失败或超时 | `connect` / `timeout` | `attempts` |
| 非成功 HTTP | `http_status` | `http_status`、`attempts` |
| HTTP 200 但 content 为空 | `empty_content` | `completion_tokens`（可用时） |
| content 不是合法 JSON | `invalid_json` | 不记录 content |
| JSON 合法但 Extract schema 不匹配 | `schema_invalid` | `schema_issue` |

这样可以定位此前多消息请求的失败类别，但不会将消息正文或模型原始输出写入日志。

## 9. 安全与隐私约束

所有日志格式和级别均不得记录：

- Bearer Token、API Key、Authorization header 或密钥环境变量值；
- 消息正文、query 原文、Evidence quote；
- 完整记忆正文、模型 prompt、模型原始 response；
- 未脱敏的 Provider 响应 body；
- 原始 tenant_id、user_id、agent_id、scope_id、conversation_id、message_id。

允许记录 query、scope、conversation、window 等内容的版本化摘要和长度/数量。即使在
`RUST_LOG=trace`、`RAM_A_LOG_SOURCE=true` 时，上述禁止项仍不得出现。

## 10. 实现建议

建议按以下边界拆分代码：

1. `memory-mcp::observability`：环境变量解析、自定义 Compact `FormatEvent`、subscriber
   构建、日志 hash 和安全摘要。
2. HTTP middleware：创建 request span，写入 `request_id` 和安全 principal hash。
3. MCP handler：将 `request_id` 传入工具错误响应。
4. 公共错误模块：提供 `ErrorOrigin`、`#[track_caller]` 构造函数和首次 origin 保留规则。
5. MemoryService：保留内部错误 cause/origin，增加 service stage 和 `pipeline_run_id` span。
6. `memory-pipeline` client/extractor/verifier：返回结构化错误类别和 origin，并记录 Provider retry。
7. `memory-core`：保留 `MemoryError` 变体和 origin，在 MCP 边界映射 embedding、SQLite 和
   vector persist 错误。

日志事件应在错误最终归属层记录一次。底层 Provider 可以记录 retry；最终失败由
Pipeline 或 Service 记录。避免同一错误在每层打印完整 ERROR，造成一条失败出现多条
无法区分的重复日志。

## 11. 自动化验收用例

建议新增或扩展以下测试。日志 subscriber 是进程级全局状态，格式和非法环境变量测试
应优先通过启动子进程执行，避免测试间互相污染。

当前自动化已经覆盖：格式配置严格解析、JSON/Compact 渲染、失败 origin 与日志打印点
区分、Pipeline 七阶段日志、模型错误分类、主要存储错误映射，以及 HTTP 响应头与
`structuredContent.request_id` 一致。以下项目仍未完整覆盖：带有效服务配置启动进程后
验证四种格式/source 组合、所有敏感字段的端到端哨兵测试、多消息窗口 hash 与
`schema_issue` 诊断、失败响应返回 `pipeline_run_id`。这些项目不能作为当前已验证能力。

| 用例 | 建议位置 | 操作 | 预期 |
| --- | --- | --- | --- |
| 默认 JSON | `crates/memory-mcp/tests/logging.rs` | 不设置两个环境变量，启动测试进程并触发普通事件和失败事件。 | 每行可解析为 JSON；普通事件无源码字段，失败事件有 error origin。 |
| Compact 切换 | 同上 | 设置 `RAM_A_LOG_FORMAT=compact`。 | 日志严格符合固定前缀顺序，错误摘要位于诊断字段之前。 |
| Compact 转义与顺序 | 同上 | message 和属性包含空格、引号、`=`、换行及乱序字段。 | 输出保持单行、可区分字段边界，已知字段和剩余字段顺序稳定。 |
| JSON 源码位置 | 同上 | 设置 `json` 和 `RAM_A_LOG_SOURCE=true`。 | `filename` 为字符串，`line_number` 为正整数。 |
| Compact 源码位置 | 同上 | 设置 `compact` 和 source=true。 | 同一行含源码文件名和行号。 |
| 错误起点 | `crates/memory-mcp/tests/logging.rs` 和各错误模块单元测试 | 分别在 source=false/true 下，于已知行通过 `#[track_caller]` 构造错误，再经过 Pipeline、Service、MCP 多层包装。 | 两种配置均输出 origin；origin 始终等于首次构造位置，不等于外层日志行；`error_site` 保持不变。 |
| 第三方错误边界 | 同上 | 注入 reqwest/rusqlite/Store 错误。 | origin 指向第一次转换为 RAM-A 错误的位置，日志不声称能定位远端服务内部行。 |
| 非法 format | 同上 | 分别设置空值、`JSON`、`text`。 | 进程非 0 退出；stderr 给出变量名和合法值。 |
| 非法 source | 同上 | 分别设置空值、`TRUE`、`1`。 | 进程非 0 退出；未打开 listener 或数据库。 |
| JSON 必要字段 | 同上 | 触发 ingest 成功、stage 失败和 search 成功事件。 | 按第 5 节逐项断言字段。 |
| 关联 ID 贯通 | `crates/memory-mcp/tests/http_mcp.rs` | 发起失败的 `memory_ingest`。 | 响应头、structuredContent 和服务日志中的 request_id 相同；run 创建后 pipeline_run_id 相同。 |
| 格式不改变行为 | 同上 | 使用静态 Extractor/Verifier 分别在 json、compact 下执行相同请求。 | 工具响应、错误码、accepted/quarantine 结果一致。 |
| Extract 空 content | `crates/memory-pipeline/src/client.rs` 与 MCP 集成测试 | mock Provider 返回 HTTP 200 且 content 为空。 | 重试后日志分类为 `empty_content`，不含响应 body。 |
| Extract 非法 JSON | `crates/memory-pipeline/tests/` | mock content 为非法 JSON。 | `stage=extract`、`source_error_kind=invalid_json`。 |
| Extract schema 错误 | 同上 | 返回错误 schema_version 或非数组 memories。 | `schema_invalid` 和对应 `schema_issue`。 |
| 多消息 Extract 失败 | MCP 集成测试 | 构造多个 candidate，mock Extractor 在单个窗口返回结构化错误。 | 记录消息/窗口数量和 hash，不记录消息正文。 |
| Embedding 失败 | `crates/memory-mcp/tests/service.rs` | 注入 `MemoryError::Embedding`。 | 响应 `EMBEDDING_FAILED`，stage 为 vector_persist 或 search_embedding。 |
| 幂等存储失败 | 同上 | 注入 reserve/complete 非 SQLite 特定错误。 | `IDEMPOTENCY_STORAGE_FAILED` 和对应 operation。 |
| SQLite busy | 同上 | 使用可控错误注入返回 busy/locked。 | `SQLITE_BUSY`、retriable=true。 |
| SQLite readonly | 同上 | 使用可控错误注入返回 readonly。 | `SQLITE_READONLY`、retriable=false。 |
| Vector persist 失败 | 同上 | embedding 成功，store 写入返回非特定错误。 | `VECTOR_PERSIST_FAILED`。 |
| STORAGE 兜底 | 同上 | 注入无法分类的存储错误。 | 保留 `STORAGE_FAILED`，日志分类为 internal。 |
| 敏感信息不落日志 | `crates/memory-mcp/tests/logging.rs` | 使用唯一哨兵作为 token、API key、消息、query、quote 和记忆正文，覆盖成功与失败路径。 | 捕获的 json/compact/trace 日志均不含任一哨兵。 |
| 第三方日志兼容 | 同上 | 触发一条 rmcp 日志。 | 日志可输出；不强制存在 fields.event，不影响 RAM-A 事件断言。 |

SQLite busy/readonly 用例优先使用错误注入或受控测试 Store，不依赖宿主文件权限和时序，
以保证 CI 稳定。另增加少量真实 SQLite 集成测试验证底层错误到分类器的映射即可。

## 12. 验收标准

满足以下条件后可认为本设计落地：

1. 两种格式和 source 开关的四种组合均通过自动化测试。
2. 非法环境变量均在业务资源初始化前导致启动失败。
3. RAM-A 自有 JSON 事件满足字段约定；Compact 可单行直接阅读。
4. source 开关的任意取值下，RAM-A 自有失败日志都包含首次错误起点和稳定
   `error_site`，多层包装不覆盖 origin；source=true 时额外包含日志打印位置。
5. MCP 工具错误可通过 `request_id` 与日志唯一关联。
6. Extract 空内容、非法 JSON、schema 错误可以只根据日志分类区分。
7. embedding、幂等存储、SQLite busy/readonly 和向量持久化错误映射符合第 8 节。
8. 两种日志格式下，同一测试输入的服务响应和 Pipeline 结果一致。
9. 敏感信息哨兵测试在 json、compact 和 trace 级别全部通过。
10. 服务自身不创建、轮转或保留日志文件。

## 13. 实施顺序与优先级

建议优先级为中等。该问题不阻断记忆摄入和检索，但直接影响容器验证、并发故障定位
和生产日志平台接入。

建议分两步实施：

1. 先实现环境变量、两种渲染格式、source 开关、事件字段、关联 ID 和脱敏测试。
2. 再保留底层 cause，完成细粒度错误码映射和 Extract/存储错误分类。

第二步会扩展 MCP 对外错误码，提交前需要同步接口规格和调用方容错说明；调用方应以
`retriable` 决定自动重试，并允许出现新增错误码。

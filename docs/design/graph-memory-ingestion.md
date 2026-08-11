# Graph memory 写入与 embedding 阶段

本文说明 graph memory 当前的写入接收和 record embedding 阶段。它是一个
feature reference：只描述已经接入的行为、状态流转和错误语义，不作为 roadmap。

## 1. 功能范围

当前路径覆盖两件事：

- `GraphRepository::accept_memory_record`：接收一条图记忆原文，保存
  `GraphMemoryRecord`，维护全文索引，并创建 `IngestionRun`。
- `GraphIngestionExecutor::process_vector_stage`：将一个 pending run 原子切换为
  `running / embedding`，获得待处理原文，生成 record embedding，保存向量索引字
  段，并把 run 推进到下一阶段入口。

写入接收完成后，返回值中的 `vector_ready` 和 `graph_ready` 都是 `false`。
`vector_ready=false` 表示接收阶段还没有生成 embedding；`graph_ready=false` 表示
当前路径还没有物化语义图。

## 2. 请求与返回值

写入请求使用 `GraphAddMemoryRequest`：

| 字段 | 作用 |
| --- | --- |
| `memory_space_id` | 图记忆空间 ID，用作隔离边界。 |
| `owner_id` | 记忆空间拥有者；已有空间的 owner 不匹配时拒绝写入。 |
| `idempotency_key` | 调用方提供的幂等键，避免重复提交同一条原文。 |
| `text` | 原始记忆文本；保存时保留原文，只用 `trim()` 判断是否为空。 |
| `metadata` | 调用方附带的结构化信息，原样序列化保存。可选 `graph_source_entity = {name, entity_type}` 用于声明原文作者/来源实体；它会形成 provenance link，不会生成 fact。 |
| `session_id` / `session_sequence` | 可选的 session 内顺序信息。 |
| `source_kind` / `source_ref` | 来源类型和来源引用。 |
| `content_role` | 文本角色，例如 user、assistant 或 system 派生内容。 |
| `created_by_agent_id` | 可选的创建 agent 标识。 |
| `observed_at_ms` | 可选的观测时间戳。 |

返回值使用 `GraphAddMemoryResponse`：

| 字段 | 作用 |
| --- | --- |
| `memory_record_id` | 新建或幂等命中的 graph memory record ID。 |
| `ingestion_run_id` | 与本次写入对应的 ingestion run ID。 |
| `status` | 当前 run 状态。新写入为 `pending`。 |
| `vector_ready` | 当前 record embedding 是否已经可用。接收阶段固定为 `false`。 |
| `graph_ready` | 当前语义图是否已经可用。接收阶段固定为 `false`。 |

## 3. 写入接收流程

`accept_memory_record` 在一个 SQLite transaction 中完成保存，避免 record、FTS
和 run 之间出现部分提交：

```text
GraphAddMemoryRequest
  -> 校验 text 非空
  -> INSERT OR IGNORE graph_memory_spaces
  -> 校验 memory_space owner
  -> 计算 stable input hash
  -> 根据 (memory_space_id, idempotency_key) 检查幂等命中
  -> 分配 memory_record_id / ingestion_run_id / ingestion_sequence
  -> 写入 graph_memory_records
  -> 写入 graph_memory_record_fts
  -> 写入 graph_ingestion_runs(status='pending', stage='accepted')
  -> commit
```

幂等规则：

- 同一个 `memory_space_id + idempotency_key` 再次写入，且 input hash 一致时，返
  回已存在的 `memory_record_id` 和 `ingestion_run_id`。
- 同一个 `memory_space_id + idempotency_key` 再次写入，但 input hash 不一致时，
  返回 `IDEMPOTENCY_CONFLICT`。

## 4. Embedding 阶段流程

`GraphIngestionExecutor::process_vector_stage` 处理单个 run：

```text
ingestion_run_id
  -> claim_pending_run
       status: pending  -> running
       stage:  accepted -> embedding  # 对本写入路径新建的 run
       attempt_count += 1
  -> embedder.embed_one(record.text)
  -> store_record_embedding
       graph_memory_records.embedding / dims / model / version 更新
       graph_ingestion_runs.stage: embedding -> extraction
  -> return Ok
```

`stage='extraction'` 表示 record embedding 已保存，run 可以进入语义处理入口；它
不表示抽取已经完成。

保存 embedding 时会记录：

- embedding BLOB；
- embedding 维度；
- `EmbeddingProvider::model_name()`；
- `graph-embedding-v1`。

## 5. 错误处理

当前路径的错误语义如下：

| 场景 | 行为 |
| --- | --- |
| `text` 为空或只有空白 | 返回 `InvalidInput`。 |
| 复用已有 `memory_space_id`，但 `owner_id` 不一致 | 返回 `MEMORY_SPACE_OWNER_MISMATCH`。 |
| 幂等键命中但 input hash 不一致 | 返回 `IDEMPOTENCY_CONFLICT`。 |
| claim 的 run 不是 `pending` | 拒绝 claim，避免重复处理。 |
| embedding provider 失败 | 优先返回原始 embedding 错误，并 best-effort 将 run 标记为 `failed`，`stage='embedding'`，`error_code='EMBEDDING_FAILED'`。 |
| embedding 保存失败 | 优先返回原始保存错误，并 best-effort 将 run 标记为 `failed`，`stage='embedding'`，`error_code='EMBEDDING_STORE_FAILED'`。 |

failed 标记失败时不会覆盖原始业务错误，调用方仍能看到 embedding 或 embedding
保存的真实失败原因。

## 6. SQLite 连接行为

Graph repository 使用独立的 SQLite 初始化路径，不影响旧的 `SqliteMemoryStore`
读写路径。文件型 SQLite 连接会设置：

```text
busy_timeout = 5000ms
journal_mode = WAL
```

`:memory:` 数据库不会启用 WAL，避免内存数据库测试环境产生不兼容行为。

当前默认图类型注册版本为 `graph-type-registry-v2`，并要求来源实体
`graph_source_entity` 才能建立 provenance link。由旧版本构建、没有来源实体链路的图数据库
不能直接视为等价的 v2 图；部署新版本前应重建 graph store，或执行经过验证的回填迁移，不能
在检索时静默退回未约束的图检索。

## 7. 测试覆盖

当前行为由以下测试覆盖：

- `graph_ingestion_acceptance.rs`：写入接收、幂等、owner 校验、原文保留。
- `graph_embedding_boundary.rs`：embedding 成功推进、embedding 失败、embedding
  保存失败后的 run 状态，以及 failed 标记失败时的原始错误保留。
- `graph_schema.rs`：graph schema 的外键、隔离边界和旧 SQLite store 解耦。

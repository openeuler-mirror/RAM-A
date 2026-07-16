# Graph memory 正式图物化阶段

本文说明 graph memory 当前的 resolution / materialization 阶段。它只描述已经实现的
候选归一、正式图写入、证据挂载、决策记录和 ingestion run 状态流转。

## 1. 功能范围

当前阶段从已经完成 extraction 的结构化候选生成正式图：

```text
running / resolution 的 IngestionRun
  -> GraphResolutionExecutor
  -> 读取最新 completed graph_extraction_runs
  -> 再次校验 GraphExtractionOutput
  -> Entity Resolution
  -> Fact Resolution
  -> 写入正式 graph_* 表
  -> IngestionRun: resolving -> completed
```

本阶段会写入正式图表，包括：

- `graph_entities`
- `graph_entity_aliases`
- `graph_facts`
- `graph_fact_evidence_groups`
- `graph_fact_evidence`
- `graph_fact_status_history`
- `graph_resolution_decisions`

本阶段不做图检索、benchmark graph mode、Neo4j 后端、复杂冲突治理、事实软失效、
事实时间线重排、community / clustering，也不使用 LLM 做归一决策。

## 2. 图结构如何形成

正式图由 Entity、Fact 和 Evidence 共同组成：

```text
Entity(subject) -- Fact(predicate + fact_text + temporal fields) -- Entity(object)
                       |
                       v
                 EvidenceGroup
                       |
                       v
                 EvidenceSpan -> GraphMemoryRecord 原文
```

`Fact` 在当前 schema 中是具有独立 ID、状态、时态字段、证据和来源的一级关系对象。
它不是普通 entity 节点，也不是只存在于内存中的临时边。这样后续可以在 fact 上叠加：

- 多条 evidence；
- active / retired 等状态；
- valid_from / valid_to；
- status history；
- FactLink 冲突、支持、替代等治理关系。

## 3. Entity Resolution v1

当前实体归一是确定性规则，不调用 LLM：

1. 对候选 `name` 做规范化：压缩空白、执行 Unicode NFC 规范化并转小写；
2. 在同一 `memory_space_id` 内按 `entity_type + normalized_name` 查找 active entity；
3. 若没命中，再按同一 entity type 的唯一 alias 查找；
4. 唯一命中则复用 entity，并补齐 alias；
5. 没有唯一命中则创建新 entity 和 alias。

数据库层会对 active entity 的 `memory_space_id + entity_type + normalized_name` 做唯一约束，
防止并发或脏数据生成重复 active entity。这个规则是 Phase 1 的保守起点：它能处理
大小写和空白差异，但不会解决同名不同人的消歧，也不会自动推理复杂别名。如果同一
alias 同时匹配多个 active entity，本阶段不会强行选择其中一个，而是创建新的 entity；
这可能产生实体碎片，后续需要通过更完整的消歧、merge 或 conflict resolution 机制治理。

## 4. Fact Resolution v1

当前事实归一也是确定性规则：

1. 先把 fact 的 `subject_ref` / `object_ref` 映射为已经归一后的 entity ID；
2. 对 `fact_text` 做规范化：压缩空白、执行 Unicode NFC 规范化并转小写；
3. 使用 `subject_entity_id + predicate + object_entity_id + normalized_fact_text` 生成
   `dedup_key`；
4. 同一 `memory_space_id` 内存在 active fact 且 `dedup_key` 相同则复用；
5. 否则创建新 fact，状态为 `active`。

数据库层会对 active fact 的 `memory_space_id + dedup_key` 做唯一约束，防止重复 active
fact。这个规则有意保守：语义相近但文本不同的事实不会在本阶段强行合并，例如
“Alice lives in Shanghai”和“Alice's home is Shanghai”会先保留为不同 fact，后续再由
更强的 conflict / equivalence / soft-retire 机制处理。

## 5. Evidence 和决策记录

每个已发布 fact 都会获得新的 evidence group：

- 新 fact 会创建 fact、status history 和 evidence；
- 复用 fact 不会重复创建 fact，但会追加新的 evidence group 和 evidence span；
- evidence span 指向当前 `GraphMemoryRecord` 原文；
- 每个 entity candidate 和 fact candidate 都会写一条 `graph_resolution_decisions`。

Evidence 的幂等粒度是一次 ingestion run。也就是说，同一个 fact 被不同 ingestion run
解析到时，每个 run 都会追加独立的 evidence group；如果同一条原文使用不同
`idempotency_key` 被重复 ingest，也会产生指向同一 record / span 的重复 evidence。
本阶段不做 `(fact_id, memory_record_id, start_byte, end_byte)` 级别的 evidence 去重。

`graph_resolution_decisions` 记录本次候选是 create 还是 reuse，method 当前固定为
`deterministic`，resolver version 当前为 `graph-resolution-v1`。

## 6. 状态流转和事务边界

成功路径：

```text
IngestionRun: running / resolution
  -> claim_resolution_run
IngestionRun: running / resolving
  -> publish_resolution
IngestionRun: completed / completed
```

`publish_resolution` 会在一个 SQLite transaction 内完成：

- entity 创建或复用；
- alias 创建；
- fact 创建或复用；
- evidence group / evidence 写入；
- status history 写入；
- resolution decision 写入；
- ingestion run 标记 completed。

如果正式图写入或 completed 状态推进失败，transaction 会回滚已写入的正式图变更。
executor 会 best-effort 把 ingestion run 标记为 `failed / resolving`，错误码为
`RESOLUTION_STORE_FAILED`，并向调用方返回原始 store 错误。

如果候选在 resolution 前再次校验失败，executor 会 best-effort 标记
`RESOLUTION_FAILED`。当前失败状态是终态；自动重试需要单独的 retry policy 和状态迁移入口。

## 7. 测试覆盖

当前行为由 `graph_resolution_materialization.rs` 覆盖：

- extraction 候选可以被物化为正式 entity、fact、evidence 和 decision；
- 成功后 ingestion run 进入 `completed / completed`；
- 第二条相同事实会复用已有 entity / fact，并追加新的 evidence；
- 正式图发布失败时会回滚 partial graph，并标记 `RESOLUTION_STORE_FAILED`。

Schema 约束由 `graph_schema.rs` 覆盖：

- extraction attempt number 在同一 ingestion run 内不能重复；
- active entity identity 不能重复；
- 同一 entity 的 active alias 不能重复；
- active fact dedup key 不能重复。

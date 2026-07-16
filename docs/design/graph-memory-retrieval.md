# Graph memory 上下文检索阶段

本文说明 graph memory 当前的 retrieval / context assembly 阶段。它描述已经实现的
图种子召回、Entity-Fact 扩展、Evidence 追溯、`ContextBundle` 组装，以及它作为可选
graph channel 接入 `MemoryManager::search(...)` 的方式。

## 1. 功能范围

当前阶段从已经完成 formal graph materialization 的图表中检索回答上下文：

```text
GraphRetrieveContextRequest
  -> 参数化 SQLite FTS seed retrieval
  -> Entity / Fact seed
  -> Entity -> Fact 或 Fact -> Entity 扩展
  -> Evidence -> GraphMemoryRecord
  -> ContextBundle
```

本阶段新增两层能力。第一层是 graph 专用上下文检索入口：

- `GraphRetrieveContextRequest`
- `GraphRepository::retrieve_context(...)`
- `ContextBundle` / `FactContextUnit` 组装

第二层是 `MemoryManager::search(...)` 的可选 graph channel：

- `RetrievalConfig.graph.enabled = false` 是默认值，关闭时 dense / BM25 / hybrid baseline
  语义不变；
- 开启后，`MemoryManager::search(...)` 会在原检索模式之外调用 graph retrieval，并把
  graph evidence record 投影回 `MemoryRecord` 结果参与融合；
- 调用方需要在 `SearchMemoryRequest.graph_memory_space_id` 中传入 graph memory space；
- `RetrievalConfig.graph.fail_open` 控制 graph channel 失败时是否退化为只返回基础检索结果。

因此 graph retrieval 不是替代现有 MemoryRecord 检索，而是作为可开关的增强召回通道接入
统一 search 入口。

本阶段不做 benchmark graph mode、LLM query rewrite、rerank、Neo4j 后端、community /
clustering、复杂时间推理、FactLink 发布，也不生成 entity / fact embedding。

## 2. 请求结构

`GraphRetrieveContextRequest` 包含：

- `memory_space_id`：检索边界。所有 seed、fact、entity、evidence 都必须在同一空间内；
- `query`：用户查询文本。当前最多 4 KiB，转换为 FTS query 时最多使用前 256 个 token；
- `top_k`：最多返回多少条 `FactContextUnit`；
- `reference_time_ms`：返回到 `ContextBundle.reference_time_ms`，未提供时使用当前时间；
- `seed_limit`：每次检索最多保留多少 seed，默认 `max(top_k * 10, 30)`；
- `max_evidence_records_per_fact`：每条 fact 最多附带多少条 evidence record，默认 3。

空白 query 会返回 `InvalidInput`。

`SearchMemoryRequest` 额外包含：

- `graph_memory_space_id`：仅当 `RetrievalConfig.graph.enabled = true` 时使用。它决定
  graph channel 在哪个 `memory_space_id` 内检索。关闭 graph channel 时该字段可为空。

`RetrievalConfig.graph` 包含：

- `enabled`：是否启用 graph channel，默认 false；
- `weight`：graph score 融合权重，默认 0.2；
- `seed_limit`：透传给 `GraphRetrieveContextRequest.seed_limit`；
- `max_evidence_records_per_fact`：透传给
  `GraphRetrieveContextRequest.max_evidence_records_per_fact`；
- `fail_open`：graph channel 出错时是否退化为空 graph 候选。默认 false，即 fail-closed。

## 3. Seed retrieval

当前 seed retrieval 使用 SQLite FTS5，不调用 LLM。

查询文本会先被转换为安全 FTS query：按空白拆分 token，每个 token 作为 quoted phrase，
再用 `OR` 连接。SQL 始终使用参数绑定，不把用户 query 拼入 SQL 字符串。

当前召回通道：

1. `graph_entity_fts`：命中 entity canonical name，生成 Entity seed；
2. `graph_entity_alias_fts`：命中 alias，映射到所属 Entity seed；
3. `graph_fact_fts`：命中 fact text，生成 Fact seed；
4. `graph_memory_record_fts`：命中原文 record，再通过 evidence 反查支持的 Fact seed。

所有通道都限制在同一 `memory_space_id`，并过滤 deleted entity / record、retired fact 和非
active fact。

Alias seed 和 record-evidence seed 会先按唯一 entity / fact 去重，再应用 `seed_limit`。
这样可以避免同一个 entity 的多个 alias 或同一个 fact 的多条 evidence record 占满
seed limit，导致其他唯一 seed 被提前截断。

## 4. 图扩展

当前扩展规则是确定性的：

```text
Entity seed
  -> active facts where subject_entity_id = entity.id
  -> active facts where object_entity_id = entity.id

Fact seed
  -> fact itself
  -> subject entity
  -> object entity
  -> evidence records
```

如果同一 fact 被多个 seed 命中，会合并为一条候选。直接 Fact seed 和 Entity seed 分数
相同时，优先保留 Fact seed 路径，因为它更直接说明 query 命中了哪条事实文本。

Entity seed 的邻接 fact 查询会使用当前 `top_k` 作为每个 entity 的扩展上限，避免高连接
entity 在小查询里拉出无界 fact 集合。最终结果仍会在全局按候选排序后截断到 `top_k`。

排序规则：

1. seed score 越高越靠前；
2. 分数相同按 fact `recorded_at_ms`；
3. 再相同按 fact id，保证输出稳定。

最终只返回前 `top_k` 条 fact context。

## 5. ContextBundle 组装

每条 `FactContextUnit` 包含：

- fact id、fact text、predicate、status；
- subject entity；
- object entity；
- evidence records；
- traversal path；
- score；
- valid time。当前只有 `valid_from_ms` 和 `valid_to_ms` 同时存在时才返回 `valid_time`。

`ContextBundle` 会同时返回去重后的：

- `records`
- `entities`
- `facts`
- `fact_links`
- `paths`

当前 formal graph 尚未发布 FactLink，因此 `fact_links` 通常为空；字段保留是为了后续
冲突、支持、替代等 fact-to-fact 治理关系接入时不改变返回结构。

## 6. MemoryManager 融合方式

`MemoryManager::search(...)` 仍先按 `RetrievalConfig.mode` 执行原有检索：

```text
Dense mode  -> dense candidates
BM25 mode   -> BM25 candidates
Hybrid mode -> dense + BM25 fusion
```

当 `RetrievalConfig.graph.enabled = true` 时，会额外执行：

```text
GraphRetrieveContextRequest
  -> ContextBundle.fact_context_units
  -> evidence_records
  -> project GraphMemoryRecord back to MemoryRecord
  -> fuse with base candidates
```

融合规则：

1. 以 `MemoryRecord.id` 去重；
2. 基础检索分数和 graph 分数分别做 min-max normalization；
3. 最终分数为 `(base_norm + graph.weight * graph_norm) / (1.0 + graph.weight)`，
   保持 score 在 `[0, 1]` 区间；
4. 同一 evidence record 被多条 fact 命中时保留最高 graph 分；
5. 返回前按最终分数降序排序，并截断到目标候选数。

如果 hybrid rerank 开启，graph channel 会先参与候选融合，再交给 reranker。这样 reranker
可以看到 graph evidence record，而不是只看到 dense/BM25 候选。

## 7. 边界和限制

- 本阶段只做图上下文检索，不直接生成自然语言答案。
- 本阶段 graph seed 只用 FTS，不使用 query embedding。
- graph channel 接入现有 search fusion，但不改变 dense / BM25 / hybrid 的默认行为。
- Evidence 返回的是 `GraphMemoryRecord`，不是逐条 evidence span 的独立返回对象；
  span 仍然保存在 `graph_fact_evidence` 表中。
- 同一 fact 的 evidence records 会按 evidence group / evidence 创建顺序返回，并受
  `max_evidence_records_per_fact` 限制。
- 无命中时返回空 `ContextBundle`，不视为 degraded。
- graph channel 只支持 SQLite store 后端。非 SQLite 后端开启 graph channel 时默认报错；
  如果 `fail_open = true`，则退化为空 graph 候选。
- graph channel 会使用 `SearchMemoryRequest.filter` 对投影后的 evidence record metadata
  再做一次过滤，保持与现有 MemoryRecord 检索的过滤语义一致。
- `graph_fact_evidence_groups(memory_space_id, fact_id)` 和
  `graph_fact_evidence(memory_space_id, evidence_group_id)` 建有 partial index，用于避免
  retrieval 热路径加载 evidence 时全表扫描。

## 8. 测试覆盖

当前行为由 `graph_retrieval_context.rs` 覆盖：

- entity/fact/query seed 能返回相关 fact context；
- fact seed 能返回 subject / object entity 和 evidence record；
- retrieval 不跨 `memory_space_id`；
- 无命中返回空 bundle；
- `top_k` 和 `max_evidence_records_per_fact` 生效。
- `MemoryManager::search(...)` 开启 graph channel 后能返回 graph evidence record；
- graph channel 缺少 `graph_memory_space_id` 时默认 fail-closed；
- `fail_open = true` 时 graph channel 缺少 `graph_memory_space_id` 会退化为空候选。
- graph repository 内部错误时的 fail-closed / fail-open 行为；
- 非 SQLite store 开启 graph channel 时的 fail-closed / fail-open 行为；
- rerank 开启时能收到 graph channel 产生的候选；
- graph FTS query token 上限和超长 query 拒绝。

`graph_repository.rs` 单元测试覆盖 FTS query builder：

- 空白输入不会产生 FTS query；
- 普通 query token 会被 quoted；
- token 内双引号会被转义。
- token 数量会被限制；
- 超长 query 会被拒绝。

`graph_schema.rs` 覆盖 evidence 相关 retrieval index 存在。

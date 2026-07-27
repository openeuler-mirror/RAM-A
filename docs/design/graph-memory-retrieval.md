# Graph memory 上下文检索阶段

本文说明 graph memory 当前的 retrieval / context assembly 阶段。它描述已经实现的
图种子召回、Entity-Fact 扩展、Evidence 追溯、`ContextBundle` 组装，以及它作为可选
graph channel 接入 `MemoryManager::search(...)` 的方式。

## 1. 功能范围

当前阶段从已经完成 formal graph materialization 的图表中检索回答上下文：

```text
GraphRetrieveContextRequest
  -> 实体锚点 / 关系词查询规划
  -> 参数化 SQLite FTS seed retrieval
  -> Entity / Fact / Evidence-record seed
  -> Entity -> Fact 兜底扩展
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

本阶段不做 LLM query rewrite、Neo4j 后端、community / clustering、复杂时间推理、
FactLink 发布或图内向量检索。`memory-bench` 可以通过显式开关调用 graph build 和 graph
retrieval；数据集 wrapper 的参数透传属于 benchmark runner 集成层。

## 2. 请求结构

`GraphRetrieveContextRequest` 包含：

- `memory_space_id`：检索边界。所有 seed、fact、entity、evidence 都必须在同一空间内；
- `query`：用户查询文本。当前最多 4 KiB，转换为 FTS query 时最多使用前 256 个 token；
- `top_k`：每类 context 最多返回多少条记录；
- `reference_time_ms`：返回到 `ContextBundle.reference_time_ms`，未提供时使用当前时间；
- `seed_limit`：每次检索最多保留多少 seed，默认 `max(top_k * 10, 30)`；
- `max_evidence_records_per_fact`：每条 fact 最多附带多少条 evidence record，默认 3。
- `query_embedding` / `query_embedding_model`：为未来独立的语义图检索提供者预留；当前 SQLite
  图检索不读取这两个字段。
- `target_subject_entity_name`：可选的结构化查询约束。提供后会解析为同一 memory space
  中的 active entity，并限制事实 subject；未提供时，图检索会从 query 中出现的 canonical
  name / alias 自动解析具有 `source_actor` 溯源关系的全部明确实体。自动锚点最多保留 8 个；
  同一位置名称无法唯一解析时跳过该位置，不强行选择。
- `target_evidence_speaker`：可选的来源实体覆盖值。它通过图实体 canonical name / alias
  解析，并以 `source_actor` 关系限制 evidence record，不读取 record 的任意业务 metadata。

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

查询文本先生成两类安全 FTS query：

1. 实体查询保留完整实义词，用于 canonical name / alias seed；
2. 事实和 evidence 查询在显式或自动解析出目标主体后移除实体名称，只保留关系、属性和
   对象词，并使用 FTS prefix term 兼容常见词形变化。

两类查询都按空白拆分 token、删除问题停用词并用 `OR` 连接。SQL 始终使用参数绑定，不把
用户 query 拼入 SQL 字符串。

当前召回通道：

1. `graph_entity_fts`：命中 entity canonical name，生成 Entity seed；
2. `graph_entity_alias_fts`：命中 alias，映射到所属 Entity seed；
3. `graph_fact_fts`：命中 fact text，生成 Fact seed；
4. `graph_memory_record_fts`：命中原文 record。它既会反查支持的 Fact seed，也会作为独立的
   evidence-record context unit 返回。

所有通道都限制在同一 `memory_space_id`，并过滤 deleted entity / record、retired fact 和非
active fact。

Alias seed 和 record-evidence seed 会先按唯一 entity / fact 去重，再应用 `seed_limit`。

自动主体解析只考虑至少作为一条 record 的 `source_actor` 出现过的实体。它先用 entity /
alias FTS 生成有界候选，再要求名称以完整 token 序列出现在 query 中；查询包含多个明确名称
时全部作为图锚点，同一位置优先最长名称，等价歧义则跳过该位置。自动锚点数硬限制为 8。
`source_actor` 只承担主体消歧和证据溯源，不作为独立候选扩张通道。
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

每个自动解析出的实体锚点都会在相同的 `seed_limit` 下执行受 subject 约束的 seed 查询，
随后按 seed 类型和 id 全局去重，并再次截断到一个 `seed_limit`，所以增加锚点不会放大最终
seed 或 context 数量。如果同一 fact 被多个 seed 命中，会合并为一条候选。直接 Fact seed 和 Entity seed 分数
相同时，优先保留 Fact seed 路径，因为它更直接说明 query 命中了哪条事实文本。只要已有
直接 Fact seed，就不再无差别扩展命中实体的全部邻接事实；Entity -> Fact 仅作为没有直接
事实匹配时的召回兜底。

Entity seed 的邻接 fact 查询会使用当前 `top_k` 作为每个 entity 的扩展上限，避免高连接
entity 在小查询里拉出无界 fact 集合。最终结果仍会在全局按候选排序后截断到 `top_k`。

排序规则：

1. seed score 越高越靠前；
2. 分数相同按 fact `recorded_at_ms`；
3. 再相同按 fact id，保证输出稳定。

最终会返回前 `top_k` 条 fact context，以及前 `top_k` 条直接 lexical evidence-record
context。候选投影为结果时按 record id 去重并再次截断；直接证据节点不会被转换为或标注为
fact。

## 5. ContextBundle 组装

每条 `FactContextUnit` 包含：

- fact id、fact text、predicate、status；
- subject entity；
- object entity；
- evidence records；
- traversal path；
- score；
- 独立的 `valid_from_ms`、`valid_to_ms` 和 `recorded_at_ms`。answer context 只展示来自
  可信时间解析器的闭合时间点或区间，避免把 LLM 推测的日期或“首次观测时间”误写成
  “事实发生时间”。

每条 `EvidenceRecordContextUnit` 包含已接受到同一 graph memory space 的原始记录、
`record:<id>` 路径、匹配类型和分数。它不含 predicate、entity
或事实时间字段，因为它是可追溯证据节点而非抽取事实。

投影到 `MemoryRecord` 后，事实支持仍写入 `metadata.graph_facts`；直接证据节点写入
`metadata.graph_matches`，其中 `kind = "evidence_record"`、`match_kind = "lexical"`。
这使评估可以分别统计图事实覆盖和原始证据覆盖，不能把后者误报为结构化事实检索能力。

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
Graph mode  -> graph facts + graph-store evidence nodes
```

当 `RetrievalConfig.graph.enabled = true` 时，会额外执行：

```text
GraphRetrieveContextRequest
  -> ContextBundle fact and evidence-record units
  -> project GraphMemoryRecord back to MemoryRecord
  -> fuse with base candidates
```

融合规则：

1. 以 `MemoryRecord.id` 去重；
2. 默认 `rerank_with_graph = false` 且 `allow_graph_only = false`：保留基础检索的候选集合和
   排序，仅把匹配的 graph facts / evidence-node metadata 合并到同一 record；
3. 显式开启 graph rerank 时，基础检索分数和 graph 分数分别做 min-max normalization，最终分数为
   `(base_norm + graph.weight * graph_norm) / (1.0 + graph.weight)`；
4. 同一 evidence record 被多条 fact 或 evidence-node 路径命中时保留最高 graph 分；
5. 显式允许 graph-only 候选时，才把未在基础结果中的图记录加入最终候选池。

如果 hybrid rerank 开启，graph channel 会先参与候选融合，再交给 reranker。这样 reranker
可以看到 graph evidence record，而不是只看到 dense/BM25 候选。

## 7. memory-bench 使用方式

`memory-bench` 的 graph 模式是显式 opt-in：

- `--graph-build`：add 阶段在普通 MemoryRecord 写入成功后，继续执行
  accept -> record embedding -> LLM extraction -> resolution，生成正式图；
- `--graph-build-concurrency`：限制同时构建的 graph record 数，默认 `1`。它只影响吞吐；
  不改变每条 record 的抽取、解析和落库逻辑；
- `--graph`：search 阶段开启 `RetrievalConfig.graph.enabled`，把 graph evidence record
  作为增强召回通道并入原有检索结果；
- `--search-mode graph`：只返回 graph store 内的 fact-grounded evidence 或直接 evidence
  node，不读取 dense/BM25 候选。该模式默认不启用，主要用于分别测量图事实与证据节点覆盖；它
  不等同于 `--graph-allow-graph-only`，后者仍是混合检索中的有限候选补充；
- `--graph-weight`：控制 graph channel 的融合权重，默认 0.2；
- `--graph-fail-open`：graph channel 出错时退化为只返回基础检索结果，默认关闭；
- `--graph-memory-space-mode auto`：prepared schema 使用 `scope_id`，raw top-level-array
  数据使用 `path:$[N]` 作为 graph memory space；单条 `--query` 没有 top-level-array path，
  因此需要通过 `--filter '{"scope_id":"..."}'` 提供 graph memory space，或者显式使用
  `metadata-field` / `path-prefix` 模式。

`--resume --graph-build` 不能只看普通 MemoryRecord 是否已存在。resume 时会继续为已存在
MemoryRecord 检查 graph build：completed ingestion run 会跳过，缺失的 ingestion run 会补构；
failed / running ingestion run 不会被自动重置，而是明确报错，避免 benchmark 在半构图状态下
静默成功。

graph build 需要真实 LLM key。默认读取 `OPENROUTER_API_KEY`，默认 base URL 是
`https://openrouter.ai/api/v1`，默认模型是 `openai/gpt-4o-mini`。可以用
`--graph-llm-api-key-env`、`--graph-llm-base-url` 和 `--graph-llm-model` 覆盖。
当 provider 的并发额度允许时，可逐步提高 `--graph-build-concurrency`；遇到限流应降低该值，
而不是改变 extraction 语义。

建议 baseline 和 graph run 使用不同 SQLite 文件，避免对比时混用已经构建过图的状态。

## 8. 边界和限制

- 本阶段只做图上下文检索，不直接生成自然语言答案。
- 当前图检索只使用 FTS、实体和事实邻接关系、entity 到 source-record 的 provenance link，以及
  fact 到原始 evidence 的可追溯链路；它不会
  扫描或重排原始 record 向量，也不会扫描 fact 向量。未来若接入图语义检索，必须以独立、可测量的
  provider 实现，不能改变 dense / BM25 / hybrid 基线。
- graph channel 接入现有 search fusion，但不改变 dense / BM25 / hybrid 的默认行为。
- 实体命中只用于补足图邻域；直接命中的 fact text 或 evidence record 始终优先，避免“提到某人”
  的实体扩展压过问题中的关系、活动或对象词。
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

## 9. 测试覆盖

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

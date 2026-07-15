# Graph memory 结构化候选抽取阶段

本文说明 graph memory 当前的 extraction candidate 阶段。它只描述已经实现的
候选抽取、候选校验、`graph_extraction_runs` 持久化和 ingestion run 状态流转。

## 1. 功能范围

当前阶段从已经完成 record embedding 的 `GraphMemoryRecord` 生成结构化图候选：

```text
running / extraction 的 IngestionRun
  -> GraphExtractionExecutor
  -> GraphExtractor
  -> GraphExtractionOutput
  -> 候选 Entity / Fact / Evidence 校验
  -> graph_extraction_runs
  -> IngestionRun stage: extracting -> resolution
```

本阶段不把候选写入正式 `graph_entities` / `graph_facts`，也不做实体归一、事实复用
或冲突处理。

## 2. Extractor 接口

抽取器实现 `GraphExtractor`：

```rust
#[async_trait::async_trait]
pub trait GraphExtractor: Send + Sync {
    fn extractor_name(&self) -> &str;
    fn model_name(&self) -> &str;
    fn prompt_version(&self) -> &str;
    fn schema_version(&self) -> &str;
    async fn extract(&self, input: GraphExtractionInput) -> MemoryResult<GraphExtractionOutput>;
}
```

`GraphExtractionExecutor` 不绑定具体 LLM provider。测试中使用 deterministic extractor；
真实 LLM adapter 可以在该 trait 后面接入。

当前 `context_record_ids` 只包含当前 `memory_record_id`。跨 record 的上下文选择需要
单独的 retrieval / context selection 步骤，本阶段不做。

## 3. 候选输出结构

`GraphExtractionOutput` 包含：

- `entities`：抽取出的实体候选；
- `facts`：抽取出的事实候选；
- `input_tokens` / `output_tokens`：可选 token 统计。

实体候选包含：

| 字段 | 作用 |
| --- | --- |
| `local_id` | 本次抽取输出内的局部实体 ID。 |
| `name` | 实体显示名称。 |
| `entity_type` | 来自 `GraphTypeRegistry` 的实体类型。 |
| `confidence` | 可选置信度，范围为 0 到 1。 |

事实候选包含：

| 字段 | 作用 |
| --- | --- |
| `local_id` | 本次抽取输出内的局部事实 ID。 |
| `subject_ref` / `object_ref` | 指向本次输出中的实体 `local_id`。 |
| `predicate` | 来自 `GraphTypeRegistry` 的 predicate。 |
| `fact_text` | 事实文本。 |
| `evidence` | 指向原文的证据 span。`start_byte` / `end_byte` 是 UTF-8 字节偏移，不是字符索引。 |
| `confidence` | 可选置信度，范围为 0 到 1。 |
| `valid_from_ms` / `valid_to_ms` | 可选有效时间区间。 |

`entities` / `facts` 允许为空，表示抽取器判断当前 record 没有可发布给后续
resolution 的图语义候选。

## 4. 候选校验

写入 `graph_extraction_runs` 前会校验结构化输出：

- entity 的 `local_id`、`name`、`entity_type` 不为空；
- entity type 必须存在于 `GraphTypeRegistry`；
- entity `local_id` 在本次输出中不能重复；
- fact 的 `subject_ref` 和 `object_ref` 必须指向本次输出中的 entity；
- predicate 必须存在于 `GraphTypeRegistry`；
- confidence 必须在 0 到 1 之间；
- `valid_from_ms` 不能晚于 `valid_to_ms`；
- 每个 fact 必须至少包含一个 evidence span；
- evidence span 必须包含 `text` 或成对的 `start_byte` / `end_byte`；
- 如果 evidence 只给出 `text`，该文本必须出现在原文中；
- evidence 的 byte range 必须在原文内，并落在 UTF-8 边界上；
- 如果 evidence 同时给出 `text` 和 byte range，二者必须匹配。

校验失败时，executor 返回 `INVALID_EXTRACTION_OUTPUT`，并 best-effort 保存 failed
extraction run。

## 5. 状态流转

成功路径：

```text
IngestionRun: running / extraction
  -> claim_extraction_run
IngestionRun: running / extracting
  -> store_extraction_success
IngestionRun: running / resolution
graph_extraction_runs.status = completed
```

失败路径：

```text
IngestionRun: running / extraction
  -> claim_extraction_run
IngestionRun: running / extracting
  -> store_extraction_failure
IngestionRun: failed / extracting
graph_extraction_runs.status = failed
```

如果结构化候选已经生成，但保存 completed extraction run 或推进 ingestion run 到
`resolution` 失败，executor 会 best-effort 保存 failed extraction run，错误码为
`EXTRACTION_STORE_FAILED`。如果 failed 记录自身也保存失败，调用方仍会收到原始
store 错误。

`stage='resolution'` 表示结构化候选已经保存，可以进入候选归一和图更新入口；它
不表示 resolution 已经完成。

当前失败状态是终态；本阶段不自动把 `failed / extracting` 重置回可重试状态。自动
重试需要单独的 retry policy 和状态迁移入口。

## 6. 测试覆盖

当前行为由 `graph_extraction_boundary.rs` 覆盖：

- 成功抽取候选并保存 `graph_extraction_runs`；
- 成功后 ingestion run 推进到 `running / resolution`；
- extractor 自身失败时保存 failed extraction run；
- 候选结构无效时返回 `INVALID_EXTRACTION_OUTPUT`；
- completed extraction run 保存失败时返回原始 store 错误，并 best-effort 记录
  `EXTRACTION_STORE_FAILED`；
- 缺失 evidence、空 evidence、脱离原文的 evidence 会被拒绝；
- 失败路径会同步标记 ingestion run 为 failed。

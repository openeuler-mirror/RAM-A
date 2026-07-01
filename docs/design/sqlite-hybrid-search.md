# SQLite 存储后端与混合检索设计

> 本文是 SQLite 存储和 hybrid retrieval 的历史设计说明。当前代码已经默认使用
> SQLite store，并支持 `dense`、`bm25`、`hybrid` 检索模式；继续保留本文是为了
> 说明设计背景、权衡和后续演进方向。
>
> 本文不再承载下一阶段 memory pipeline 设计。长对话 chunk、结构化 memory
> extraction、timeline-aware retrieval/reasoning 等后续方向见
> [memory-pipeline-roadmap.md](memory-pipeline-roadmap.md)。

## 1. 背景与问题

早期实现通过 `FileMemoryStore` 使用 JSONL 文件作为存储后端。这个设计简单、透明，适合 smoke test，但当记忆数量变大时，读写和检索成本会迅速上升。

当前存储接口如下：

```rust
pub trait MemoryStore: Send + Sync {
    async fn add_record(&self, record: &MemoryRecord) -> MemoryResult<()>;
    async fn list_records(&self) -> MemoryResult<Vec<MemoryRecord>>;
    async fn replace_all(&self, records: &[MemoryRecord]) -> MemoryResult<()>;
}
```

当前记忆记录结构如下：

```rust
pub struct MemoryRecord {
    pub id: String,
    pub text: String,
    pub metadata: serde_json::Value,
    pub embedding: Vec<f32>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}
```

问题不在于 JSONL 不能用。JSONL 可读、易调试，也很适合小规模本地测试。真正的问题是当前 JSONL 实现会反复加载并重写整个存储文件。

例如，当前新增一条记录大致是：

```text
读取整个 memory.jsonl
把每一行反序列化成 Vec<MemoryRecord>
移除相同 id 的旧记录
追加新记录
重写整个文件
```

当前检索大致是：

```text
读取整个 memory.jsonl
把所有记录反序列化到内存
按 metadata 过滤
对所有剩余记录计算 cosine similarity
排序全部结果
截断到 top_k
```

即使 `top_k = 20`，实现也可能在返回 20 条结果之前扫描所有已存记忆。对于 6,425 条 PersonaMem 记录，这仍然可接受。但对于更大的数据集或真实用户记忆库，全量扫描和全量重写会带来更高的内存占用、磁盘 IO 和延迟。

一个粗略的内存估算：

```text
1024 维 embedding = 1024 个 f32
1024 * 4 bytes = 4096 bytes，约 4 KB
100,000 条记录 = 仅 embedding 向量就约 400 MB
```

这个估算还没有包含文本、metadata、JSON 解析开销、堆对象开销和临时缓冲区。JSON 会把浮点数存成文本，因此磁盘文件也会明显大于紧凑二进制表示。

查询路径还有质量问题。当前检索是纯 dense vector retrieval：先把 query 编码成向量，再与每条记录 embedding 做 cosine similarity，最后返回分数最高的结果。Dense retrieval 擅长语义相似，但可能漏掉姓名、日期、标题、罕见词和明确否定约束等精确词面线索。PersonaMem 中的 `suggest_new_ideas` 和 `track_full_preference_evolution` 等类别会暴露这类弱点。

本设计引入：

- 基于 `rusqlite` 的 SQLite 存储后端。
- 多后端存储配置，让 JSONL 继续可用。
- 结合 dense embedding search 与 BM25 text search 的 hybrid retrieval，默认权重为 `0.7 / 0.3`。

## 2. 目标与非目标

### 目标

1. 增加一个 SQLite 本地存储后端。
2. 保留 JSONL，继续作为 debug 和 smoke test 后端。
3. 通过配置或 CLI 选择后端，而不是硬编码单一存储实现。
4. 使用结构化列和紧凑 embedding 存储格式保存记忆记录。
5. 支持 benchmark 隔离所需的 metadata 过滤，尤其是 `scope_id`。
6. 使用 SQLite FTS5 增加 BM25 关键词检索。
7. 增加 hybrid retrieval：

```text
final_score = 0.7 * dense_score_norm + 0.3 * bm25_score_norm
```

8. 尽量保持现有 benchmark 输出格式不变。
9. 补充足够测试，证明 JSONL 行为没有回退，SQLite 行为与其兼容。

### 非目标

1. 第一版 SQLite 不实现生产级 ANN 向量索引。
2. 不移除 JSONL 存储。
3. 本次不引入图数据库支持。
4. 不重设计完整 public memory API。
5. 第一版不支持任意嵌套 metadata 过滤表达式。
6. 不承诺所有 benchmark 都提升准确率。Hybrid retrieval 预期能提升鲁棒性和可解释性，但仍需要实际评估。

## 3. 当前实现瓶颈

### 3.1 Add 路径瓶颈

当前 `FileMemoryStore::add_record` 实际上近似于：

```rust
let mut records = self.list_records().await?;
records.retain(|existing| existing.id != record.id);
records.push(record.clone());
self.replace_all(&records).await
```

这意味着单条 add 的成本会随着整个 store 大小增长：

```text
cost(add_one) ~= O(number_of_records)
```

如果 benchmark 逐条写入 100,000 条记忆，重复的全量读写会变得很昂贵。

当前 `MemoryManager::add_many_with_batch_size` 已经更好，因为它会批量 embedding。但它仍然会加载所有已有记录，并在最后调用 `replace_all`。这对 JSONL 兼容性可以接受，但对数据库后端并不理想。

### 3.2 Search 路径瓶颈

当前 search 会调用：

```rust
let records = self.store.list_records().await?;
```

然后遍历每条符合条件的记录：

```text
检查 metadata filter
检查 embedding 维度
计算 cosine similarity
收集所有带分记录
排序所有记录
截断 top_k
```

成本为：

```text
memory = O(number_of_records)
cpu = O(number_of_records * embedding_dims)
sort = O(number_of_records * log(number_of_records))
```

对于 1024 维 embedding，每个候选比较都有明显 CPU 成本。

### 3.3 存储抽象瓶颈

现有 `MemoryStore` trait 更像文件接口：

```text
列出所有记录
替换所有记录
```

数据库更适合操作式接口：

```text
upsert 单条记录
在事务中 upsert 多条记录
按 filter 拉取候选
运行 BM25 search
运行后端特定的 candidate search
```

第一版 SQLite 可以先实现当前 trait 以保持兼容，但后续阶段应避免让所有后端都被迫通过 `list_records` 工作。

## 4. 存储后端设计

### 4.1 后端类型

引入后端枚举：

```rust
pub enum StoreBackendKind {
    Jsonl,
    Sqlite,
}
```

CLI 示例：

```bash
cargo run -p memory-bench -- \
  --store-backend sqlite \
  --store data/personalmem/personalmem_32k.sqlite \
  ...
```

配置示例：

```json
{
  "storage": {
    "backend": "sqlite",
    "path": "data/personalmem/personalmem_32k.sqlite"
  }
}
```

Benchmark runner 应通过 factory 构建后端：

```rust
fn build_store(config: &StoreConfig) -> Result<Arc<dyn MemoryStore>> {
    match config.backend {
        StoreBackendKind::Jsonl => Ok(Arc::new(FileMemoryStore::new(&config.path))),
        StoreBackendKind::Sqlite => Ok(Arc::new(SqliteMemoryStore::open(&config.path)?)),
    }
}
```

### 4.2 兼容策略

第一阶段继续使用现有 trait：

```rust
Arc<dyn MemoryStore>
```

SQLite 实现：

```text
add_record
list_records
replace_all
```

这样可以很容易把现有测试同时跑在 JSONL 和 SQLite 上。

第二阶段增加后端感知的检索能力：

```rust
#[async_trait]
pub trait SearchableMemoryStore: MemoryStore {
    async fn dense_candidates(
        &self,
        query_embedding: &[f32],
        filter: Option<&serde_json::Value>,
        limit: usize,
    ) -> MemoryResult<Vec<ScoredMemory>>;

    async fn bm25_candidates(
        &self,
        query: &str,
        filter: Option<&serde_json::Value>,
        limit: usize,
    ) -> MemoryResult<Vec<TextScoredMemory>>;
}
```

这样可以避免 SQLite 长期被迫通过 `list_records()` 做检索。

### 4.3 为什么保留 JSONL

JSONL 应继续保留，因为它：

- 便于人工检查。
- 适合很小的 smoke test。
- 适合回归测试。
- 在 SQLite 依赖或 FTS5 不可用时可以作为 fallback。

目标不是删除 JSONL，而是停止让 JSONL 成为唯一存储形态。

## 5. SQLite Schema

### 5.1 主表

建议表结构：

```sql
CREATE TABLE IF NOT EXISTS memories (
  id TEXT PRIMARY KEY,
  text TEXT NOT NULL,
  metadata_json TEXT NOT NULL,
  embedding BLOB NOT NULL,
  embedding_dims INTEGER NOT NULL,
  scope_id TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);
```

字段映射：

| 字段 | 来源 | 原因 |
|---|---|---|
| `id` | `MemoryRecord.id` | 稳定主键，支持 resume/upsert |
| `text` | `MemoryRecord.text` | 原始记忆文本 |
| `metadata_json` | `MemoryRecord.metadata` | 保留完整 metadata |
| `embedding` | `MemoryRecord.embedding` | 紧凑存储向量 |
| `embedding_dims` | `embedding.len()` | 校验 query/record 维度 |
| `scope_id` | 如果存在则取 `metadata.scope_id` | 常见 benchmark/user 隔离过滤字段 |
| `created_at_ms` | `MemoryRecord.created_at_ms` | 保留创建时间 |
| `updated_at_ms` | `MemoryRecord.updated_at_ms` | 保留更新时间 |

推荐索引：

```sql
CREATE INDEX IF NOT EXISTS idx_memories_scope_id ON memories(scope_id);
CREATE INDEX IF NOT EXISTS idx_memories_updated_at_ms ON memories(updated_at_ms);
```

为什么把 `scope_id` 从 JSON metadata 中复制成独立列？

PersonaMem 和 LoCoMo 通常需要用 user/session 类字段隔离结果。查询：

```sql
WHERE scope_id = ?
```

比反复执行下面的 JSON 表达式更简单也更快：

```sql
json_extract(metadata_json, '$.scope_id') = ?
```

完整 metadata 仍然保存在 `metadata_json` 中。

### 5.2 用于 BM25 的 FTS 表

使用 SQLite FTS5：

```sql
CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts
USING fts5(id UNINDEXED, text);
```

Upsert 时同步维护：

```sql
INSERT OR REPLACE INTO memories (...)
VALUES (...);

DELETE FROM memory_fts WHERE id = ?;
INSERT INTO memory_fts(id, text) VALUES (?, ?);
```

BM25 candidate query：

```sql
SELECT id, bm25(memory_fts) AS bm25_raw
FROM memory_fts
WHERE memory_fts MATCH ?
ORDER BY bm25(memory_fts)
LIMIT ?;
```

注意：SQLite FTS5 的 `bm25()` 排序方向需要谨慎处理。通常按 `bm25(...)` 升序排序时，较低 raw value 可能表示更相关。与 dense score 合并前，应先转换成“越高越好”的归一化分数。

### 5.3 Embedding 序列化

将 `Vec<f32>` 存为 `BLOB`。

转换方式：

```text
Vec<f32> -> little-endian bytes -> SQLite BLOB
SQLite BLOB -> 每 4 bytes 一组 -> f32 values
```

为什么用 BLOB 而不是 JSON？

对于 1024 维向量：

```text
BLOB size = 1024 * 4 bytes = 4096 bytes
JSON float array = 更大的文本表示
```

BLOB 可以减少 JSON 浮点解析，也能降低文件大小。

## 6. 多后端配置设计

### 6.1 CLI 参数

新增：

```text
--store-backend jsonl|sqlite
--store PATH
```

保留当前 `--store` 参数。默认值：

```text
--store-backend sqlite
--store data/memory.sqlite
```

JSONL 示例：

```bash
cargo run -p memory-bench -- \
  --store-backend jsonl \
  --store data/personalmem/memory.jsonl \
  add \
  --dataset data/personalmem/prepared/personalmem_32k_v1.json
```

SQLite 示例：

```bash
cargo run -p memory-bench -- \
  --store-backend sqlite \
  --store data/personalmem/memory.sqlite \
  add \
  --dataset data/personalmem/prepared/personalmem_32k_v1.json
```

### 6.2 Search Mode 参数

新增：

```text
--search-mode dense|bm25|hybrid
--embedding-weight 0.7
--bm25-weight 0.3
--candidate-k 100
```

默认值：

```text
search_mode = hybrid
embedding_weight = 0.7
bm25_weight = 0.3
candidate_k = max(top_k * 5, 100)
```

JSONL/dense 兼容路径仍可显式开启：

```bash
--store-backend jsonl \
--search-mode dense
```

### 6.3 未来配置文件形态

长期可以使用：

```json
{
  "storage": {
    "backend": "sqlite",
    "path": "data/personalmem/personalmem_32k.sqlite"
  },
  "retrieval": {
    "mode": "hybrid",
    "candidate_k": 100,
    "embedding_weight": 0.7,
    "bm25_weight": 0.3
  },
  "embedding": {
    "provider": "openrouter",
    "model": "baai/bge-m3",
    "dimensions": 1024
  }
}
```

## 7. Hybrid Retrieval 算法

### 7.1 Dense Retrieval

Dense retrieval：

```text
query text -> embedding model -> query vector
query vector vs memory vectors -> cosine similarity
取 top dense candidates
```

示例：

```text
Query: "Recommend a plant-based dinner place."

Memory A: "User loves vegan restaurants."
Memory B: "User bought a new laptop."

cosine(query, A) = 0.86
cosine(query, B) = 0.21
```

即使用词不同，dense retrieval 也能找到语义相关记忆：

```text
vegan ~= plant-based
```

### 7.2 BM25 Retrieval

BM25 retrieval：

```text
query terms -> FTS index -> keyword relevance score
```

示例：

```text
Query: "Pacific Islander melodies remix album"
```

Memory A：

```text
User released a remix album blending electronic music with Pacific Islander melodies.
```

Memory B：

```text
User likes music.
```

BM25 会强烈偏向 Memory A，因为它匹配罕见且具体的词：

```text
Pacific
Islander
melodies
remix
album
```

### 7.3 Candidate Union

不要简单返回 dense top-k 加 BM25 top-k。应使用更大的候选池：

```text
top_k = 20
candidate_k = max(top_k * 5, 100)
```

流程：

```text
1. Dense search 返回 top candidate_k。
2. BM25 search 返回 top candidate_k。
3. 按 memory id 合并候选。
4. 给每个候选分配 dense 和 BM25 分数。
5. 缺失 dense score 记为 0。
6. 缺失 BM25 score 记为 0。
7. 分别归一化两类分数。
8. 计算 weighted final score。
9. 按 final score 排序。
10. 返回 top_k。
```

示例：

```text
Dense candidates: A, B, C, D
BM25 candidates:  C, D, E, F
Union:            A, B, C, D, E, F
```

这样，即使关键词很强的记忆 `E` 没有进入 dense top candidates，也能进入最终排序。

### 7.4 默认权重

默认：

```text
embedding_weight = 0.7
bm25_weight = 0.3
```

公式：

```text
final_score = 0.7 * dense_score_norm + 0.3 * bm25_score_norm
```

示例：

```text
Candidate A:
dense_score_norm = 0.80
bm25_score_norm = 0.50
final_score = 0.7 * 0.80 + 0.3 * 0.50 = 0.71

Candidate B:
dense_score_norm = 0.70
bm25_score_norm = 0.95
final_score = 0.7 * 0.70 + 0.3 * 0.95 = 0.775
```

Candidate B 排名更高，因为它仍有语义相关性，同时关键词证据更强。

## 8. 分数归一化

### 8.1 为什么必须归一化

Cosine score 和 BM25 score 不在同一个尺度上。

Dense cosine 常见范围：

```text
0.20 to 0.95
```

Raw BM25 可能是：

```text
0 to 18
```

SQLite FTS5 的 `bm25()` 还可能方向相反，也就是数值越低越相关。因此下面的做法是错误的：

```text
0.7 * raw_cosine + 0.3 * raw_bm25
```

因为 BM25 可能仅凭数值尺度就主导最终分数。

### 8.2 推荐归一化方式

第一版建议对每个 query 的 candidate set 做 min-max normalization。

对于越高越好的分数：

```text
norm = (score - min_score) / (max_score - min_score)
```

如果所有分数相同：

```text
有该类分数的候选：norm = 1.0
缺失该类分数的候选：norm = 0.0
```

对于 SQLite FTS5 BM25，如果数值越低越好：

```text
bm25_goodness = max_bm25_raw - bm25_raw
bm25_norm = normalize(bm25_goodness)
```

示例：

```text
dense raw:
A = 0.82
B = 0.74
C = 0.60

dense normalized:
A = 1.00
B = 0.64
C = 0.00
```

BM25 raw，假设转换后已经是越高越好：

```text
A = 2
B = 10
C = 6

bm25 normalized:
A = 0.00
B = 1.00
C = 0.50
```

最终：

```text
A = 0.7 * 1.00 + 0.3 * 0.00 = 0.70
B = 0.7 * 0.64 + 0.3 * 1.00 = 0.748
C = 0.7 * 0.00 + 0.3 * 0.50 = 0.15
```

### 8.3 Score 字段

当前 `ScoredMemory` 只有一个 score：

```rust
pub struct ScoredMemory {
    pub record: MemoryRecord,
    pub score: f32,
}
```

为了 API 兼容，`score` 可以继续表示最终分数。Hybrid search 内部应跟踪：

```rust
struct HybridCandidate {
    record: MemoryRecord,
    dense_raw: Option<f32>,
    bm25_raw: Option<f32>,
    dense_norm: f32,
    bm25_norm: f32,
    final_score: f32,
}
```

为了调试，可以可选地在 metadata 中包含：

```json
{
  "retrieval": {
    "dense_score": 0.82,
    "bm25_score": 0.64,
    "final_score": 0.766
  }
}
```

这个字段应保持可选，避免破坏现有输出消费方。

## 9. 迁移计划

### Phase 0: 设计评审

交付本设计文档，并确认：

- 后端 CLI 名称。
- SQLite schema。
- Dense search 是否继续作为默认值。
- `hybrid` 是否先作为 opt-in。
- SQLite 中应索引哪些 metadata 字段。

### Phase 1: 增加依赖和 SQLite Store 骨架

增加：

```toml
rusqlite = { version = "...", features = ["bundled"] }
```

也可以选择增加：

```toml
bytemuck = "..."
```

或者手写 f32 到 bytes 的转换。

创建：

```text
crates/memory-core/src/sqlite_store.rs
```

实现：

```text
SqliteMemoryStore::open(path)
initialize_schema()
add_record()
list_records()
replace_all()
```

此阶段现有 dense search 应该可以工作，且 benchmark 行为不变。

### Phase 2: 后端选择

更新 `memory-bench`：

```text
--store-backend jsonl|sqlite
```

Runtime 构建从：

```rust
let store = Arc::new(FileMemoryStore::new(&cli.store));
```

改为：

```rust
let store = build_store(&cli)?;
```

早期迁移方案曾考虑默认保持：

```text
jsonl
```

当前实现已经将默认后端切到 SQLite；需要旧行为时显式传 `--store-backend jsonl`。

### Phase 3: SQLite BM25 Candidate Search

增加 FTS 表和查询路径：

```text
memory_fts
bm25_candidates(query, filter, limit)
```

先使用简单 query escaping/tokenization。如果原始 query 触发 FTS 语法错误，则 fallback 到 sanitized token query。

### Phase 4: Hybrid Search

增加 retrieval config：

```text
--search-mode dense|bm25|hybrid
--embedding-weight 0.7
--bm25-weight 0.3
--candidate-k 100
```

实现：

```text
dense candidates
bm25 candidates
union
normalize
weighted rerank
top_k
```

### Phase 5: PersonaMem Smoke 和完整评估

运行：

```text
PersonaMem 32k smoke
PersonaMem 32k full add/search
answer/grade with ctx2k
by-question-type analysis
```

对比：

```text
jsonl dense
sqlite dense
sqlite hybrid
```

### Phase 6: 可选 Store Migration Tool

如有需要，可以增加：

```bash
cargo run -p memory-bench -- migrate-store \
  --from-backend jsonl \
  --from data/personalmem/memory.jsonl \
  --to-backend sqlite \
  --to data/personalmem/memory.sqlite
```

这一步是可选的，因为 benchmark 数据通常可以重新 ingest。

## 10. 测试计划

### 10.1 Store Conformance Tests

让同一组测试同时覆盖 JSONL 和 SQLite：

```text
add one record
add duplicate id replaces old record
list records returns all records
replace_all replaces all records
metadata is preserved
embedding is preserved exactly enough for cosine search
created_at_ms and updated_at_ms are preserved
```

### 10.2 SQLite 专项测试

```text
schema initializes on empty DB
scope_id is extracted from metadata
scope_id index supports filtered query
embedding BLOB round-trips Vec<f32>
FTS table updates on insert/replace
BM25 query returns keyword match
```

示例：

```text
Memory A: "User loves Pacific Islander melodies."
Memory B: "User bought running shoes."
Query: "Pacific melodies"
Expected BM25 top result: Memory A
```

### 10.3 Dense Search Tests

现有测试应继续通过：

```text
add_then_search_returns_relevant_memory
search respects metadata filter
embedding dimension mismatch returns error
```

这些测试应同时覆盖两个后端。

### 10.4 Hybrid Search Tests

构造确定性候选：

```text
A: strong dense, weak BM25
B: medium dense, strong BM25
C: weak dense, weak BM25
```

验证：

```text
final_score = 0.7*dense_norm + 0.3*bm25_norm
ordering follows final_score
missing dense or BM25 score is handled as 0
top_k is respected
```

### 10.5 CLI Tests

```text
memory-bench --store-backend jsonl add/search
memory-bench --store-backend sqlite add/search
invalid backend returns a clear error
hybrid mode on jsonl either works with fallback BM25 or returns a clear unsupported error
```

### 10.6 Benchmark Smoke Tests

PersonaMem smoke：

```bash
python3 evaluation/personalmem/run.py prepare \
  --size 32k \
  --limit-questions 5 \
  --max-context-messages 50 \
  --schema-version benchmark-prepared-v1 \
  --prepared-dataset data/personalmem/prepared/personalmem_32k_v1_smoke.json

cargo run -p memory-bench -- \
  --store-backend sqlite \
  --store data/personalmem/personalmem_32k_smoke.sqlite \
  --embedding openrouter \
  --model baai/bge-m3 \
  --dimensions 1024 \
  add \
  --dataset data/personalmem/prepared/personalmem_32k_v1_smoke.json

cargo run -p memory-bench -- \
  --store-backend sqlite \
  --store data/personalmem/personalmem_32k_smoke.sqlite \
  --embedding openrouter \
  --model baai/bge-m3 \
  --dimensions 1024 \
  --search-mode hybrid \
  --embedding-weight 0.7 \
  --bm25-weight 0.3 \
  search \
  --dataset data/personalmem/prepared/personalmem_32k_v1_smoke.json \
  --output outputs/personalmem_32k_v1_sqlite_hybrid_smoke_search_results.json \
  --top-k 20
```

### 10.7 性能指标

记录：

```text
add duration
search duration
peak memory if available
store file size
empty result count
scope mismatch count
QA Accuracy
Avg Context Tokens
by-question-type accuracy
```

## 11. 风险与权衡

### 11.1 rusqlite 是同步库

`rusqlite` 是同步库。当前代码使用 async traits 和 Tokio。如果在 async task 中直接执行阻塞 SQLite 操作，可能阻塞 Tokio worker threads。

缓解方式：

1. 用 `tokio::task::spawn_blocking` 包裹 SQLite 操作。
2. 第一版保持 SQLite 操作短小，并用事务限制范围。
3. 如果后续并发需求上升，再考虑专用 DB worker。

第一版建议：

```text
对 list_records、replace_all、dense candidate loading、BM25 search 等较重操作使用 spawn_blocking
```

### 11.2 SQLite 不是完整向量数据库

SQLite 改善了持久化和过滤，但 dense search 可能仍然需要在 Rust 中扫描候选 embedding。这对第一版本地后端可以接受，但不应描述为 ANN vector search。

未来选项：

```text
sqlite-vec
Qdrant
pgvector
Milvus
```

### 11.3 FTS Query Syntax 可能失败

用户 query 可能包含 FTS5 会特殊解释的标点或运算符。

缓解：

```text
sanitize query tokens
fallback to plain token query
如果配置允许，BM25 candidates 返回空而不是让整个 search 失败
```

### 11.4 分数归一化可能影响排序

Min-max normalization 简单，但容易受 outlier 影响。第一版可接受，因为它易解释、易测试。

未来替代方案：

```text
rank-based fusion
reciprocal rank fusion
z-score normalization
learned reranking
```

### 11.5 Hybrid 可能降低某些结果

Hybrid search 可以提升精确匹配鲁棒性，但也可能引入关键词很强的干扰项。

示例：

```text
Query: "new restaurant I have not tried before"
```

BM25 可能召回包含以下词的记忆：

```text
restaurant
tried
before
```

其中一些记忆可能描述的是已经去过的旧餐厅，应该被排除。最终 answer model 必须正确理解它们。因此 benchmark 结果必须测量，不能预设。

### 11.6 Metadata Filter 范围

第一版支持简单对象相等过滤：

```json
{"scope_id": "abc"}
```

不要过早设计嵌套查询语言。如果后续确实需要，再定义正式 filter AST。

### 11.7 向后兼容

默认命令使用 SQLite/hybrid：

```bash
cargo run -p memory-bench -- \
  --store data/personalmem/memory.sqlite \
  add \
  --dataset ...
```

这意味着：

```text
default backend = sqlite
default search mode = hybrid
```

脚本也显式选择 SQLite/hybrid；需要旧行为时可传 `--store-backend jsonl --search-mode dense`。

## 12. 推荐第一批实现切片

先实现最小可用的纵向切片：

```text
1. Add StoreBackendKind and --store-backend.
2. Add SqliteMemoryStore with schema initialization.
3. Implement add_record/list_records/replace_all.
4. Make existing dense search work on SQLite.
5. Add tests proving JSONL and SQLite return equivalent dense search results.
```

再实现 hybrid：

```text
6. Add memory_fts table.
7. Keep FTS table in sync on add/replace.
8. Add BM25 candidate search.
9. Add --search-mode hybrid.
10. Add score normalization and 7:3 weighted fusion.
11. Run PersonaMem smoke and full comparison.
```

这个顺序可以降低风险：先验证 SQLite 后端正确性，再改变检索排序逻辑。

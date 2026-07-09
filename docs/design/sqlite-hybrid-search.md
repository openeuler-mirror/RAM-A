# SQLite 存储后端与混合检索

本文说明 RAM-A 当前的 SQLite 存储后端，以及基于 dense、BM25 和 hybrid 的候选
检索路径。

## 1. 功能范围

SQLite 后端负责：

- 持久化 `MemoryRecord`。
- 按 ID upsert 单条或批量记忆。
- 维护 FTS5 文本索引。
- 在 SQLite 侧完成 dense candidate 排序。
- 提供 BM25 candidate 查询。
- 支持 `scope_id` 等 metadata filter 的常见过滤路径。

JSONL 后端仍保留，用于小规模 smoke test 和人工检查；默认 benchmark 路径使用
SQLite。

## 2. Store 接口

当前存储 trait：

```rust
#[async_trait]
pub trait MemoryStore: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    async fn add_record(&self, record: &MemoryRecord) -> MemoryResult<()>;
    async fn add_records(&self, records: &[MemoryRecord]) -> MemoryResult<()>;
    async fn list_records(&self) -> MemoryResult<Vec<MemoryRecord>>;
    async fn replace_all(&self, records: &[MemoryRecord]) -> MemoryResult<()>;
}
```

`add_records` 是批量 upsert 接口。`SqliteMemoryStore` 在单个 SQLite transaction 中
写入整批记录，避免 manager 为每批数据执行 `list_records()` 和 `replace_all()`。

## 3. SQLite Schema

主表：

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

CREATE INDEX IF NOT EXISTS idx_memories_scope_id
ON memories(scope_id);
```

FTS 表：

```sql
CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts
USING fts5(id UNINDEXED, text);
```

字段说明：

| 字段 | 说明 |
| --- | --- |
| `id` | 记忆主键，支持 upsert。 |
| `text` | 原始记忆文本。 |
| `metadata_json` | 完整 metadata JSON。 |
| `embedding` | little-endian `f32` BLOB。 |
| `embedding_dims` | embedding 维度，用于查询校验。 |
| `scope_id` | 从 `metadata.scope_id` 提取出的常用过滤列。 |
| `created_at_ms` | 创建时间。 |
| `updated_at_ms` | 更新时间。 |

`scope_id` 仍然保存在 `metadata_json` 中；独立列用于常见 benchmark/user 隔离查询。

## 4. 写入路径

单条写入使用 `add_record`：

```text
open SQLite connection
-> INSERT ... ON CONFLICT(id) DO UPDATE
-> refresh memory_fts row
```

批量写入使用 `add_records`：

```text
open SQLite connection
-> begin transaction
-> upsert each MemoryRecord
-> refresh memory_fts rows
-> commit
```

SQLite 操作通过 blocking task 执行，避免在 async runtime 中直接执行同步数据库
操作。

## 5. Dense Candidate 检索

SQLite 后端接入 `sqlite-vec`。Dense candidate 查询在 SQLite 中计算 cosine distance
并排序：

```sql
SELECT
    id,
    text,
    metadata_json,
    embedding,
    embedding_dims,
    created_at_ms,
    updated_at_ms,
    vec_distance_cosine(vec_f32(embedding), vec_f32(?1)) AS distance
FROM memories
WHERE embedding_dims = ?2
ORDER BY distance ASC, id ASC
LIMIT ?3;
```

如果 filter 中包含 `scope_id`，查询会增加：

```sql
AND scope_id = ?3
```

这样 dense-only 和 hybrid 中的 dense 分支都不需要把全部 embedding 拉到 Rust 里计
算相似度。非 SQLite 后端仍使用 `list_records()` 的兼容路径。

## 6. BM25 Candidate 检索

BM25 使用 SQLite FTS5：

```sql
SELECT
    memories.id,
    memories.text,
    memories.metadata_json,
    memories.embedding,
    memories.embedding_dims,
    memories.created_at_ms,
    memories.updated_at_ms,
    bm25(memory_fts) AS bm25_raw
FROM memory_fts
JOIN memories ON memories.id = memory_fts.id
WHERE memory_fts MATCH ?1
ORDER BY bm25(memory_fts)
LIMIT ?2;
```

SQLite FTS5 的 `bm25()` 原始值越低通常表示越相关。RAM-A 会将该分数归一化为
“越高越好”的 `score`，再参与 hybrid fusion。

## 7. Hybrid Fusion

`SearchMode::Hybrid` 的流程：

```text
1. 生成 query embedding。
2. 从 store 获取 dense candidates。
3. 从 SQLite FTS 获取 BM25 candidates。
4. 按 memory id 合并候选。
5. 分别归一化 dense score 和 BM25 score。
6. 计算 weighted final score。
7. 如果 rerank 关闭，返回 top_k。
8. 如果 rerank 开启，先截断到 rerank.input_k，再进入 rerank 阶段。
```

默认权重：

```text
embedding_weight = 0.7
bm25_weight = 0.3
```

融合公式：

```text
final_score = embedding_weight * dense_norm + bm25_weight * bm25_norm
```

`candidate_k` 默认由 `RetrievalConfig::candidate_limit(top_k)` 计算：

```text
candidate_k = max(top_k * 5, 100)
```

也可以在 CLI 中显式指定。

## 8. Metadata Filter

`scope_id` 是常用过滤字段：

```json
{"scope_id": "conversation-7"}
```

SQLite 查询会优先使用 `memories.scope_id` 缩小候选范围。查询结果返回 Rust 后，还
会通过 `record.rs` 中的 `metadata_matches` 做完整 filter 校验，保证与非 SQLite
后端语义一致。

## 9. CLI 使用

SQLite + hybrid 示例：

```bash
cargo run -p memory-bench -- \
  --store-backend sqlite \
  --store data/personalmem/memory.sqlite \
  --search-mode hybrid \
  --embedding-weight 0.7 \
  --bm25-weight 0.3 \
  add \
  --dataset data/personalmem/prepared/personalmem_32k_v1.json
```

```bash
cargo run -p memory-bench -- \
  --store-backend sqlite \
  --store data/personalmem/memory.sqlite \
  --search-mode hybrid \
  --candidate-k 100 \
  search \
  --dataset data/personalmem/prepared/personalmem_32k_v1.json \
  --top-k 20 \
  --output outputs/personalmem_search_results.json
```

JSONL 兼容路径：

```bash
cargo run -p memory-bench -- \
  --store-backend jsonl \
  --store data/personalmem/memory.jsonl \
  --search-mode dense \
  search \
  --query "What does the user like?" \
  --top-k 5
```

## 10. 代码位置

| 路径 | 作用 |
| --- | --- |
| `crates/memory-core/src/store.rs` | `MemoryStore` trait 和 JSONL store。 |
| `crates/memory-core/src/sqlite_store.rs` | SQLite schema、upsert、dense candidates、BM25 candidates。 |
| `crates/memory-core/src/manager.rs` | search mode 分发、hybrid fusion、rerank 接入点。 |
| `crates/memory-core/src/record.rs` | `scope_id` 提取和 `metadata_matches`。 |
| `crates/memory-bench/src/main.rs` | `--store-backend`、`--search-mode`、`--candidate-k` 等 CLI 参数。 |

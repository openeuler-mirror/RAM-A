# memory-cases Storage Split

`memory-cases` uses two logical stores:

- Business DB (`--rag-store`): source-of-truth RAG state.
- Document/vector index (`--memory-store`): derived retrieval index built by `memory-core`.

The current local implementation uses SQLite for both stores, but keeps them in separate
files by default so the ownership boundary is explicit.

## Runtime Paths

| `ram-a-mem` configuration | Default | Purpose |
| --- | --- | --- |
| `case_library.rag_store` | `data/memory-cases.sqlite` | Business DB for datasets, documents, tasks, and chunks |
| `case_library.index_store` | `data/memory-cases-index.sqlite` | Retrieval index DB for memory text, FTS rows, and vectors |

Both settings must be ordinary filesystem paths. SQLite URI filenames such as
`file:data/memory-cases.sqlite?mode=rw` are not supported.

`memory-cases` is a library embedded by `ram-a-mem`; it no longer owns a standalone API or
ingestor process. The case-management API and ingestion worker share one in-process
`RagService` and therefore always use the same configured store pair. There is no
single-file case store setting.

## Table Ownership

| Table | Store | Role | Ownership |
| --- | --- | --- | --- |
| `rag_datasets` | Business DB | Dataset metadata and scope | Source of truth |
| `rag_documents` | Business DB | Uploaded document metadata, file path, status | Source of truth |
| `rag_tasks` | Business DB | Ingestion task lifecycle | Source of truth |
| `rag_chunks` | Business DB | Parsed and chunked document content | Canonical chunk content |
| `memories` | Memory index | `memory-core` records: text, metadata, embedding | Maintained by ingest/update/delete |
| `memory_fts` | Memory index | FTS5 copy of `memories.text` | Maintained with `memories` |

Document retrieval records in `memories` carry `memory_index_namespace = "memory-cases"`.
The recommended runtime layout keeps this memory index DB separate from the RAM-A
long-term memory DB. Separate SQLite files avoid write-lock contention between the
case-library service and the RAM-A MCP service, and make case reindex/reset operations
safe without touching user memories.

If a small smoke test intentionally shares this memory index DB with another RAM-A service,
all writers for the same search scope must use one embedding profile: provider, base URL,
model, API key environment name, and dimensions. `memory-core` stores this profile on new
records and rejects same-scope profile mismatches. This protects against both dimension
mismatches and the subtler case where two models have the same dimensions but incompatible
vector spaces. Shared SQLite index deployment is not recommended for concurrent services.

`rag_chunks.content` and `memories.text` are intentionally different:

- `Chunk.content` is the chunk text after parser and chunker processing. It preserves the
  user-facing text used by `/chunks`, `/search` references, and chat references.
- `memories.text` is search-index text generated from `Chunk.content` or a document summary.
  It is normalized through `build_search_index_text`, which extracts lexical tokens and
  joins them with spaces for BM25/dense retrieval. If token extraction yields nothing, it
  falls back to trimmed text.
- `memories.embedding` is generated from `memories.text`, not directly from `Chunk.content`.

## Ingestion Flow

1. The case-management API embedded in `ram-a-mem` writes uploaded file metadata and a
   pending task to the Business DB.
2. The background ingestion task in the same `ram-a-mem` process leases a pending task
   from `rag_tasks`.
3. Ingestor parses the source file and writes processed chunks into `rag_chunks`.
4. Ingestor builds memory records from those chunks:
   - one record per chunk, with `record_kind = "chunk"`;
   - one document-summary record per document, with `record_kind = "document_summary"`.
5. `memory-core` embeds `memories.text` and writes `memories` plus `memory_fts`.
6. Task and document status are marked completed in the Business DB.

The Business DB is the durable source for document state and chunk content. The memory
index is maintained incrementally by ingestion, document update, and document delete flows.
Runtime operations never create a missing Business DB or index DB. A missing Business DB is
reported to MCP callers as `CASE_BUSINESS_DATABASE_MISSING`; a missing index is reported as
`CASE_INDEX_DATABASE_MISSING`. The ingestion worker keeps checking storage at the configured
poll interval, tracks both database files independently, logs each newly missing database
immediately, and rate-limits its repeated logs instead of starting an automatic rebuild.

During daemon startup, an absent index is initialized only when the Business DB contains no
canonical chunks. If canonical chunks already exist, the missing state is preserved so an empty
replacement cannot hide data loss, including after a daemon restart.

An administrator can explicitly start a rebuild with `POST /api/v1/index/rebuilds` and poll
`GET /api/v1/index/rebuilds/{operation_id}` using the case-management administrator bearer token.
The asynchronous job regenerates all chunk and document-summary records into the sibling
`<index>.rebuild.tmp` database, checkpoints it, runs SQLite `quick_check`, and verifies both the
memory and FTS record counts. Only then does it briefly exclude index readers and atomically
rename the temporary database over the configured index. Case mutations and ingestion are paused
while the business snapshot is rebuilt; searches continue against the previous complete index,
or return `CASE_INDEX_DATABASE_MISSING` when no previous index exists. A failed or interrupted
rebuild never exposes the temporary database as the live index. Starting a new rebuild cleans an
orphaned temporary database left by an interrupted process.

## Why Split

Keeping all case-library state in one SQLite file couples business state with a replaceable
retrieval index. Sharing the case index with RAM-A long-term memory also couples two
independent services to the same SQLite writer lock. Splitting gives clearer failure
handling:

- Backups prioritize the Business DB and uploaded source files.
- Document index writes and cleanup can be scoped without touching user long-term memories.
- Future backends can replace `--memory-store` with Elasticsearch, Qdrant, Milvus, Infinity,
  or another document/vector engine without moving the business schema.
- Tests can exercise document update/delete cleanup without deleting datasets, documents,
  tasks, chunks, or user long-term memories stored in another DB.

# memory-cases Storage Split

`memory-cases` uses two logical stores:

- Business DB (`--rag-store`): source-of-truth RAG state.
- Document/vector index (`--memory-store`): derived retrieval index built by `memory-core`.

The current local implementation uses SQLite for both stores, but keeps them in separate
files by default so the ownership boundary is explicit.

## Runtime Paths

| Argument | Default | Purpose |
| --- | --- | --- |
| `--rag-store` | `data/memory-cases.sqlite` | Business DB for datasets, documents, tasks, and chunks |
| `--memory-store` | `data/memory-cases-index.sqlite` | Retrieval index DB for memory text, FTS rows, and vectors |

API and ingestor processes must use the same pair of `--rag-store` and `--memory-store`
paths. `memory-cases` does not provide a single-file store argument.

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
User long-term memories can live in the same physical memory DB as long as they do not use
that namespace.

`rag_chunks.content` and `memories.text` are intentionally different:

- `Chunk.content` is the chunk text after parser and chunker processing. It preserves the
  user-facing text used by `/chunks`, `/search` references, and chat references.
- `memories.text` is search-index text generated from `Chunk.content` or a document summary.
  It is normalized through `build_search_index_text`, which extracts lexical tokens and
  joins them with spaces for BM25/dense retrieval. If token extraction yields nothing, it
  falls back to trimmed text.
- `memories.embedding` is generated from `memories.text`, not directly from `Chunk.content`.

## Ingestion Flow

1. API writes uploaded file metadata and a pending task to the Business DB.
2. Ingestor leases a pending task from `rag_tasks`.
3. Ingestor parses the source file and writes processed chunks into `rag_chunks`.
4. Ingestor builds memory records from those chunks:
   - one record per chunk, with `record_kind = "chunk"`;
   - one document-summary record per document, with `record_kind = "document_summary"`.
5. `memory-core` embeds `memories.text` and writes `memories` plus `memory_fts`.
6. Task and document status are marked completed in the Business DB.

The Business DB is the durable source for document state and chunk content. The memory
index is maintained incrementally by ingestion, document update, and document delete flows.
`memory-cases` intentionally does not expose a memory database rebuild command or API, because
the same `memory-store` may also contain user long-term memories.

## Why Split

Keeping everything in one SQLite file is acceptable for a small smoke test, but it couples
business state with a replaceable retrieval index. Splitting gives clearer failure handling:

- Backups prioritize the Business DB and uploaded source files.
- Document index writes and cleanup can be scoped without touching user long-term memories.
- Future backends can replace `--memory-store` with Elasticsearch, Qdrant, Milvus, Infinity,
  or another document/vector engine without moving the business schema.
- Tests can exercise document update/delete cleanup without deleting datasets, documents,
  tasks, chunks, or user long-term memories stored in the same memory DB.

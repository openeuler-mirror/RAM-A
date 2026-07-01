# RAM-A Memory Pipeline Roadmap

> Status: planning note for post-first-import work.
>
> Decision: keep `sqlite-hybrid-search.md` as a historical design note for the
> already implemented SQLite store and dense/BM25/hybrid retrieval path. Do not
> compress it into an ADR during the first import. The next architecture work is
> broader than storage and retrieval, so it should live here as the memory
> pipeline roadmap.

## 1. Why This Is Separate From SQLite Hybrid Search

SQLite hybrid search answers a narrower question: how RAM-A stores memory records
locally and retrieves candidates with dense embeddings, BM25, or a weighted
hybrid ranker.

The next set of problems sits before and after retrieval:

- how raw long conversations become stable memory units;
- how raw dialogue is compressed into durable long-term memories;
- how changing user preferences, facts, and events are represented over time;
- how retrieval and answer generation respect the latest state and temporal
  constraints.

These concerns should not be folded into the SQLite design document. They may
use the SQLite store, but they are pipeline-level capabilities rather than
storage-backend decisions.

## 2. Design Principles

1. Keep benchmark scoring standards and output schemas stable unless a benchmark
   owner explicitly approves a new version.
2. Keep `memory-core` benchmark-agnostic. Dataset-specific raw fields should stay
   in `evaluation/<dataset>/` adapters.
3. Preserve provenance from generated memories back to source messages, chunks,
   timestamps, and speakers.
4. Treat extraction and temporal reasoning as optional pipeline stages at first,
   so the current add/search baseline remains runnable.
5. Prefer metadata extensions over breaking `MemoryRecord` and prepared-schema
   consumers in the first iteration.

## 3. Layer 1: Conversation Chunking

Goal: split long conversations into more stable memory chunks before ingestion.
This should reduce retrieval noise and make each memory unit better suited for
recall and reranking.

Initial chunk boundaries should support:

- `speaker`: group or split by speaker and role when role matters;
- time window: keep events close in time together and avoid crossing large gaps;
- topic: split when the conversation clearly shifts topic;
- token length: enforce max and target token windows for model and retrieval
  stability.

Expected output:

```json
{
  "id": "conversation-7:chunk-4",
  "text": "User discussed preferring quiet vegan restaurants near work...",
  "metadata": {
    "scope_id": "conversation-7",
    "chunk_id": "conversation-7:chunk-4",
    "source_turn_ids": ["turn-21", "turn-22", "turn-23"],
    "speakers": ["user", "assistant"],
    "start_time": "2024-05-01T10:03:00Z",
    "end_time": "2024-05-01T10:08:00Z",
    "topic": "restaurants",
    "token_count": 184
  }
}
```

First implementation should be deterministic and testable with small fixtures.
LLM-based topic segmentation can come later after rule-based chunking has a
stable interface.

## 4. Layer 2: Semantic Memory Extraction

Goal: avoid storing only raw dialogue. RAM-A should extract structured long-term
memory candidates from chunks, then store concise memories with enough metadata
to support retrieval, reranking, and answer grounding.

Initial memory types:

- `preference`: user likes, dislikes, habits, constraints, and goals;
- `fact`: stable user facts or environment facts;
- `relationship`: people, organizations, and relationship context;
- `event`: things that happened at a point or interval in time;
- `state_update`: changes to preferences, plans, status, or availability.

Expected output:

```json
{
  "id": "conversation-7:chunk-4:memory-2",
  "text": "User prefers quiet vegan restaurants near work.",
  "metadata": {
    "scope_id": "conversation-7",
    "memory_type": "preference",
    "subject": "user",
    "predicate": "prefers",
    "object": "quiet vegan restaurants near work",
    "source_chunk_id": "conversation-7:chunk-4",
    "confidence": 0.86
  }
}
```

The extraction stage should use the same RAM-A OpenAI-compatible LLM client and
local JSON parsing conventions already introduced for LoCoMo judge calls. It
should be unit-testable with a stubbed model response before running any real
API-backed benchmark.

## 5. Layer 3: Timeline-Aware Memory Reasoning

Goal: represent and retrieve memory in a way that handles preference evolution,
latest state, and time-constrained questions.

Core metadata:

- `event_time`: when the remembered event happened;
- `observed_at`: when RAM-A learned the memory;
- `valid_from` and `valid_to`: validity interval when known;
- `supersedes`: older memory IDs replaced or weakened by this memory;
- `status`: `active`, `superseded`, `expired`, or `uncertain`;
- `temporal_confidence`: confidence in the temporal interpretation.

Target cases:

- user used to prefer A, later changed to B;
- question asks for the latest preference or current state;
- question asks about a specific time window;
- multiple memories conflict and need temporal reranking.

Initial retrieval strategy:

1. retrieve candidate memories using existing dense/BM25/hybrid search;
2. apply optional temporal filters from query metadata when available;
3. rerank active and recent state updates above superseded records;
4. expose provenance and temporal metadata to the answer layer.

This can later evolve into timeline-aware reasoning, but the first version should
stay simple enough to test with deterministic fixtures.

## 6. Suggested Implementation Order

1. Add a deterministic chunking module and fixture tests.
2. Teach prepared dataset adapters to optionally emit chunked memories while
   preserving current benchmark outputs.
3. Add semantic extraction behind an explicit flag and test it with stubbed LLM
   responses.
4. Extend memory metadata with temporal fields without breaking existing
   `MemoryRecord` consumers.
5. Add temporal rerank as an opt-in retrieval stage.
6. Run PersonaMem and LoCoMo smoke comparisons before enabling the new pipeline
   for full benchmark runs.

## 7. Open Questions

- Should extracted memories and raw chunks share the same store, or should they
  be separated by `memory_kind = raw_chunk|extracted_memory`?
- Which temporal fields should become first-class Rust structs versus generic
  metadata in the first implementation?
- Should extraction run at add time only, or should RAM-A support offline
  re-extraction when prompts or schemas change?
- How should benchmark reports compare raw-chunk retrieval against extracted
  memory retrieval without changing existing scoring schemas?

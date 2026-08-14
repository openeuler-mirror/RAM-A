# Memory Benchmark Prepared Schema v1

This document defines the unified prepared dataset format for memory benchmark adapters in RAM-A. The format is intended for PersonaMem, LoCoMo, LongMemEval, and future benchmarks that need to run through the same add/search baseline.

## Quick Start: Adding a New Benchmark Adapter

新增 benchmark 时，原则上只需要新增一个数据集适配器：

```text
evaluation/{dataset}/run.py
```

adapter 的第一步不是直接改 `memory-core` 或 `memory-bench`，而是把原始数据转换成统一的 `benchmark-prepared-v1`：

```json
{
  "schema_version": "benchmark-prepared-v1",
  "dataset": {
    "name": "locomo",
    "split": "test",
    "source": "locomo"
  },
  "memories": [],
  "queries": []
}
```

隔离字段约定：

- `memories[].metadata.scope_id`：写入 memory 时使用的隔离字段。
- `queries[].filter.scope_id`：search 时限制检索范围的隔离字段。

例如，同一个用户、同一个 session、同一个 conversation、同一个 document 下的 memories 和 queries 应该使用相同的 `scope_id`。这样检索时只会在对应 scope 内搜索，不会把其他用户、其他会话或其他文档的 memory 混进来。

使用 `memory-bench add/search` 的基本命令：

```bash
cargo run -p memory-bench -- \
  --store data/locomo_store.sqlite \
  --store-backend sqlite \
  --search-mode hybrid \
  --embedding hash \
  add \
  --dataset data/locomo/prepared/locomo_v1.json
```

```bash
cargo run -p memory-bench -- \
  --store data/locomo_store.sqlite \
  --store-backend sqlite \
  --search-mode hybrid \
  --embedding hash \
  search \
  --dataset data/locomo/prepared/locomo_v1.json \
  --output outputs/locomo_search_results.json \
  --top-k 20
```

验证 scope 没串的 Python 脚本：

```python
import json

results = json.load(open("outputs/locomo_search_results.json", encoding="utf-8"))

bad_queries = []
for item in results:
    expected_scope = (item.get("filter") or {}).get("scope_id")
    if not expected_scope:
        bad_queries.append({
            "query_id": item.get("query_id"),
            "reason": "missing query filter.scope_id",
        })
        continue

    bad_results = []
    for result in item.get("results", []):
        actual_scope = (result.get("metadata") or {}).get("scope_id")
        if actual_scope != expected_scope:
            bad_results.append({
                "memory_id": result.get("id"),
                "expected_scope": expected_scope,
                "actual_scope": actual_scope,
            })

    if bad_results:
        bad_queries.append({
            "query_id": item.get("query_id"),
            "bad_results": bad_results,
        })

print("bad queries =", len(bad_queries))
if bad_queries:
    print(json.dumps(bad_queries[:5], ensure_ascii=False, indent=2))
    raise SystemExit(1)
```

禁止事项：

- 不要为了单个 benchmark 修改 `memory-core`。
- 不要在 `memory-bench` 里硬编码数据集原始字段。
- 不要对需要 scope 隔离的数据集做全局混搜。

接入 checklist：

- [ ] `schema_version` 是 `benchmark-prepared-v1`。
- [ ] `memories[]` 每条都有 `text`。
- [ ] `memories[]` 每条都有 `metadata.scope_id`。
- [ ] `queries[]` 每条都有 `text`。
- [ ] `queries[]` 每条都有 `filter.scope_id`。
- [ ] `add` 后 store 里的 memory metadata 正确。
- [ ] `search` 后 `bad queries = 0`。

## 1. Design Goals

`memory-core` should remain benchmark-agnostic. It only provides generic long-term memory capabilities:

- add memory
- search memory
- filter search by metadata

`memory-bench` should act as the unified CLI runner. It should know how to add and search a prepared benchmark dataset, but it should not understand the raw fields of PersonaMem, LoCoMo, LongMemEval, or any other specific benchmark.

`evaluation/{dataset}/run.py` is the dataset adapter layer. Each adapter is responsible for reading the original benchmark files and converting them into the unified prepared schema v1.

Different benchmarks have different raw formats, but they should all be converted into the same intermediate structure:

- `memories[]` for records that should be written to memory
- `queries[]` for benchmark questions or retrieval queries

Adding a new benchmark should not require changes to `memory-core`. In principle, it should also not require changes to `memory-bench` just because the original dataset uses different field names.

## 2. Prepared Schema v1 Structure

The schema version must be:

```json
"benchmark-prepared-v1"
```

Example:

```json
{
  "schema_version": "benchmark-prepared-v1",
  "dataset": {
    "name": "personamem",
    "split": "32k",
    "source": "bowen-upenn/PersonaMem"
  },
  "memories": [
    {
      "id": "ctx1:0",
      "text": "User: I like jazz.",
      "metadata": {
        "dataset": "personamem",
        "scope_id": "ctx1",
        "shared_context_id": "ctx1",
        "role": "user",
        "speaker": "user",
        "turn_index": 0,
        "conversation_index": 0
      }
    }
  ],
  "queries": [
    {
      "id": "q1",
      "text": "What music does the user like?",
      "filter": {
        "scope_id": "ctx1"
      },
      "metadata": {
        "question_type": "preference",
        "topic": "music"
      },
      "task": {
        "type": "multiple_choice",
        "answer_options": ["(a) rock", "(b) jazz", "(c) classical", "(d) pop"],
        "correct_answer": "(b)"
      }
    }
  ]
}
```

## 3. Field Reference

### `schema_version`

Required string. For this version, it must be `benchmark-prepared-v1`.

The runner can use this value to distinguish the new prepared schema from legacy JSON files that rely on recursive field scanning.

### `dataset.name`

Required string. The normalized benchmark name.

Examples:

- `personamem`
- `locomo`
- `longmemeval`

### `dataset.split`

Optional string. The benchmark split, size, or variant.

Examples:

- `32k`
- `128k`
- `full`
- `dev`
- `test`

### `dataset.source`

Optional string. The upstream dataset name, URL, repository, or local source identifier.

Examples:

- `bowen-upenn/PersonaMem`
- `locomo`
- `longmemeval`

### `memories[].id`

Optional string. Stable memory identifier generated by the adapter.

If omitted, `memory-core` may generate an ID. For benchmark reproducibility, adapters should prefer stable IDs.

Examples:

- `ctx1:0`
- `conversation-7:turn-12`
- `user-3:doc-2:chunk-4`

### `memories[].text`

Required string. The text that should be added to long-term memory.

The adapter should decide whether to include role prefixes such as `User:` or `Assistant:`. If role information may affect retrieval or answering, it should also be preserved in `metadata`.

### `memories[].metadata`

Optional object. Metadata stored together with the memory record.

Common fields:

- `dataset`: benchmark name
- `scope_id`: generic isolation scope
- `shared_context_id`: original PersonaMem shared context ID, if applicable
- `conversation_id`: original conversation ID, if applicable
- `session_id`: original session ID, if applicable
- `user_id`: original user ID, if applicable
- `document_id`: original document ID, if applicable
- `role`: normalized role such as `user`, `assistant`, or `system`
- `speaker`: original speaker field from the dataset
- `graph_source_entity`: optional generic provenance declaration with `name` and registered
  `entity_type`; graph ingestion links the source record to this entity without creating a fact
- `turn_index`: turn index inside a conversation or session
- `conversation_index`: legacy PersonaMem-style conversation index

`memory-core` treats metadata as generic JSON. Dataset-specific meaning belongs to the adapter and evaluation layer.

### `queries[].id`

Optional string. Stable query or question identifier.

Examples:

- `q1`
- `personamem-question-42`
- `locomo-session-7-question-3`

### `queries[].text`

Required string. The query text passed to memory search.

For QA benchmarks, this is usually the question. For retrieval-only benchmarks, this may be the retrieval query.

### `queries[].filter`

Optional object. Metadata filter passed to search.

The most common field is:

```json
{
  "scope_id": "ctx1"
}
```

Filters are used to restrict search to the relevant user, session, conversation, document, or benchmark-defined memory scope.

### `queries[].metadata`

Optional object. Query-level metadata needed for reporting, grouping, or downstream scoring.

Examples:

- `question_type`
- `topic`
- `category`
- `persona_id`
- `difficulty`
- `source_row`

These fields should not be required by `memory-core`.

### `queries[].task.type`

Optional string. The evaluation task type.

Recommended values:

- `multiple_choice`
- `open_qa`
- `retrieval`
- `classification`

Dataset adapters can add new task types when needed, but answer and grade logic must document how each type is scored.

### `queries[].task.answer_options`

Optional array of strings. The candidate answer choices for multiple-choice tasks.

Example:

```json
["(a) rock", "(b) jazz", "(c) classical", "(d) pop"]
```

Required when `task.type` is `multiple_choice`.

### `queries[].task.correct_answer`

Optional string. The gold answer used by the dataset-specific grader.

For multiple-choice tasks, this should use the normalized option label when possible, such as `(a)`, `(b)`, `(c)`, or `(d)`.

For open QA tasks, this may be a text answer or a dataset-specific answer object. The dataset adapter and grader are responsible for interpreting it.

## 4. `scope_id` Convention

`scope_id` is the generic isolation field for benchmark memory search.

It normalizes dataset-specific isolation concepts into one common field:

- PersonaMem: `shared_context_id` -> `scope_id`
- LoCoMo: `conversation_id` or `session_id` -> `scope_id`
- LongMemEval: `user_id`, `document_id`, or benchmark-defined memory scope -> `scope_id`

The convention is:

- `memories[].metadata.scope_id` is written into memory during add.
- `queries[].filter.scope_id` is used during search to restrict retrieval.

Benchmarks that require user, session, conversation, or document isolation should not search the full global memory pool unless the benchmark explicitly defines global search as the expected behavior.

Without scope isolation, unrelated memories from other users, sessions, or documents may enter the top-k results and distort both retrieval metrics and answer accuracy.

## 5. Adapter Responsibilities

Each `evaluation/{dataset}/run.py` should own dataset-specific behavior.

The adapter should:

- read the raw benchmark files
- convert raw examples into prepared schema v1
- map dataset-specific fields into `memories[].metadata`
- map dataset-specific search constraints into `queries[].filter`
- preserve reporting and scoring fields in `queries[].metadata` or `queries[].task`
- call `memory-bench add` and `memory-bench search`
- implement dataset-specific answer and grade logic, such as multiple-choice parsing or open-QA scoring

The adapter may keep dataset-specific commands such as download, prepare, answer, grade, and report generation.

## 6. `memory-bench` Responsibilities

`memory-bench` should be the generic CLI runner for prepared benchmark datasets.

For schema v1, `memory-bench add` should:

- read `memories[].text`
- read `memories[].metadata`
- pass them to `AddMemoryRequest`
- use `memories[].id` when present

For schema v1, `memory-bench search` should:

- read `queries[].text`
- read `queries[].filter`
- pass them to `SearchMemoryRequest`
- preserve query metadata in the output when useful for downstream evaluation

`memory-bench` should not understand raw PersonaMem, LoCoMo, or LongMemEval field names.

It should continue to support legacy behavior:

- `--query`
- `--filter`
- `--text-fields`
- `--query-fields`
- recursive JSON field scanning for old prepared files

## 6.1 Upsert Behavior

When `memories[].id` matches an existing record in the store, the new record replaces the old one (upsert). This is intentional for benchmark re-runs: running `add` twice with the same prepared dataset produces the same store.

Adapters should ensure each memory has a globally unique, stable `id`. Do not reuse the same `id` for different memory content (e.g., different turns, different users). If two unrelated memories share an `id`, the second write silently replaces the first.

## 7. Prohibited Patterns

Do not modify `memory-core` for a single benchmark.

Do not hard-code benchmark-specific raw field names in `memory-bench`, such as PersonaMem-only `shared_context_id` extraction or LoCoMo-only session parsing, once schema v1 is adopted.

Do not place dataset-specific scope logic inside `memory-core`.

Do not run global memory-pool search for benchmarks that require scope isolation, unless the benchmark explicitly defines global search as the correct protocol.

Do not use benchmark-specific answer parsing or grading logic inside `memory-core` or generic memory storage/search components.

## 8. Compatibility Strategy

The current legacy JSON recursive scanning behavior should remain temporarily available.

Legacy add behavior:

- recursively scan fields such as `text`, `content`, `message`, and `memory`

Legacy search behavior:

- recursively scan fields such as `question` and `query`
- support direct `--query`
- support global `--filter`

New benchmark adapters should output schema v1 by default.

PersonaMem currently has a legacy prepared format based on:

- `conversation[]`
- `questions[]`

This format will be migrated gradually to schema v1:

- `conversation[]` -> `memories[]`
- `questions[]` -> `queries[]`

During migration, adapters may temporarily emit both legacy fields and schema v1 fields to avoid breaking existing commands.

## 9. Benchmark Mapping Examples

### PersonaMem

Memory mapping:

| Raw PersonaMem field | Prepared schema v1 field |
| --- | --- |
| `conversation[].id` | `memories[].id` |
| `conversation[].text` | `memories[].text` |
| `conversation[].shared_context_id` | `memories[].metadata.scope_id` |
| `conversation[].shared_context_id` | `memories[].metadata.shared_context_id` |
| `conversation[].speaker` | `memories[].metadata.role` |
| `conversation[].speaker` | `memories[].metadata.speaker` |
| conversation array index | `memories[].metadata.conversation_index` |

Query mapping:

| Raw PersonaMem field | Prepared schema v1 field |
| --- | --- |
| `questions[].question_id` | `queries[].id` |
| `questions[].question` | `queries[].text` |
| `questions[].shared_context_id` | `queries[].filter.scope_id` |
| `questions[].question_type` | `queries[].metadata.question_type` |
| `questions[].topic` | `queries[].metadata.topic` |
| `questions[].all_options` | `queries[].task.answer_options` |
| `questions[].correct_answer` | `queries[].task.correct_answer` |

PersonaMem is a scope-isolated benchmark. Search should normally use:

```json
{
  "scope_id": "<shared_context_id>"
}
```

### LoCoMo

Expected memory mapping:

| Raw LoCoMo concept | Prepared schema v1 field |
| --- | --- |
| conversation or session ID | `memories[].metadata.scope_id` |
| conversation/session ID | `memories[].metadata.conversation_id` or `session_id` |
| turns/messages | `memories[]` |
| message role | `memories[].metadata.role` |
| turn index | `memories[].metadata.turn_index` |

Expected query mapping:

| Raw LoCoMo concept | Prepared schema v1 field |
| --- | --- |
| QA/query ID | `queries[].id` |
| QA/query text | `queries[].text` |
| conversation/session ID | `queries[].filter.scope_id` |
| question type/category | `queries[].metadata` |
| gold answer/evidence | `queries[].task.correct_answer` or task-specific fields |

LoCoMo adapters should decide whether `scope_id` represents a full conversation, a session, or a user-level scope according to the benchmark protocol.

### LongMemEval

Expected memory mapping:

| Raw LongMemEval concept | Prepared schema v1 field |
| --- | --- |
| user ID | `memories[].metadata.user_id` |
| document ID | `memories[].metadata.document_id` |
| benchmark memory scope | `memories[].metadata.scope_id` |
| long-term records/documents/chunks | `memories[]` |
| record timestamp/order | `memories[].metadata.turn_index` or task-specific metadata |

Expected query mapping:

| Raw LongMemEval concept | Prepared schema v1 field |
| --- | --- |
| evaluation question ID | `queries[].id` |
| evaluation question text | `queries[].text` |
| user/document/scope constraint | `queries[].filter.scope_id` |
| task category | `queries[].metadata` |
| gold answer/evidence | `queries[].task.correct_answer` or task-specific fields |

LongMemEval may use user-level, document-level, or benchmark-defined scopes. The adapter should normalize the chosen search boundary into `scope_id` and document the mapping.

from __future__ import annotations

import copy

import pytest

from common.memory_pipeline.canonical import estimate_tokens, stable_hash
from common.memory_pipeline.normalize import normalize_prepared_memories


def test_stable_hash_uses_canonical_mapping_order() -> None:
    assert stable_hash({"a": 1, "b": 2}) == stable_hash({"b": 2, "a": 1})
    assert len(stable_hash("value")) == 24


def test_estimate_tokens_counts_cjk_words_and_punctuation_deterministically() -> None:
    assert estimate_tokens("Alice 去杭州。") == 5
    assert estimate_tokens("a_b") == 3
    assert estimate_tokens("  ") == 0


def test_normalize_prepared_memories_preserves_source_fields() -> None:
    prepared = {
        "schema_version": "benchmark-prepared-v1",
        "memories": [
            {
                "id": "m1",
                "text": "I moved to Hangzhou.",
                "metadata": {
                    "scope_id": "u1",
                    "session_id": "s1",
                    "role": "user",
                    "speaker": "Alice",
                    "turn_idx": 3,
                    "session_date": "2026-07-01",
                },
            }
        ],
        "queries": [],
    }
    original = copy.deepcopy(prepared)

    messages, issues = normalize_prepared_memories(prepared)

    assert issues == []
    assert len(messages) == 1
    assert messages[0].id == "m1"
    assert messages[0].scope_id == "u1"
    assert messages[0].session_id == "s1"
    assert messages[0].role == "user"
    assert messages[0].speaker == "Alice"
    assert messages[0].turn_index == 3
    assert messages[0].timestamp == "2026-07-01"
    assert messages[0].source_index == 0
    assert messages[0].metadata["session_date"] == "2026-07-01"
    assert prepared == original


def test_normalize_reports_invalid_rows_without_losing_valid_rows() -> None:
    prepared = {
        "schema_version": "benchmark-prepared-v1",
        "memories": [
            {"id": "missing-scope", "text": "bad", "metadata": {}},
            {"id": "blank", "text": "  ", "metadata": {"scope_id": "u1"}},
            {"id": "ok", "text": "valid", "metadata": {"scope_id": "u1"}},
        ],
        "queries": [],
    }

    messages, issues = normalize_prepared_memories(prepared)

    assert [message.id for message in messages] == ["ok"]
    assert [issue.code for issue in issues] == ["missing_scope_id", "blank_source_message"]
    assert [issue.source_id for issue in issues] == ["missing-scope", "blank"]


def test_normalize_rejects_wrong_prepared_schema() -> None:
    with pytest.raises(ValueError, match="benchmark-prepared-v1"):
        normalize_prepared_memories({"schema_version": "legacy", "memories": []})


def test_normalize_rejects_duplicate_message_ids() -> None:
    prepared = {
        "schema_version": "benchmark-prepared-v1",
        "memories": [
            {"id": "same", "text": "first", "metadata": {"scope_id": "u1"}},
            {"id": "same", "text": "second", "metadata": {"scope_id": "u1"}},
        ],
    }

    with pytest.raises(ValueError, match="duplicate source message id: same"):
        normalize_prepared_memories(prepared)

from __future__ import annotations

import json
from pathlib import Path

import pytest

from locomo.locomo_adapter import prepare_locomo
from locomo.locomo_provenance import (
    QueryRef,
    query_ref,
    render_contexts,
    result_evidence_ids,
)


FIXTURE = Path(__file__).parents[1] / "fixtures" / "locomo_sample.json"
DATASET = json.loads(FIXTURE.read_text(encoding="utf-8"))
PREPARED = prepare_locomo(DATASET)


def test_query_ref_prefers_stable_query_id() -> None:
    item = {
        "query_id": "S0:Q1",
        "task": {"sample_index": 99, "question_index": 99},
    }

    assert query_ref(item) == QueryRef(sample_index=0, question_index=1)


def test_result_evidence_ids_use_one_raw_id_or_unique_extracted_refs() -> None:
    assert result_evidence_ids({"id": "S0:D1:0"}, "raw") == ("S0:D1:0",)
    result = {
        "id": "mem-1",
        "metadata": {
            "evidence_refs": [
                {"message_id": "S0:D1:0"},
                {"message_id": "S0:D1:0"},
                {"message_id": "S0:D1:1"},
            ]
        },
    }
    assert result_evidence_ids(result, "extracted") == (
        "S0:D1:0",
        "S0:D1:1",
    )


def test_extracted_context_expands_unique_source_turns_in_result_order() -> None:
    item = {
        "query_id": "S0:Q0",
        "task": {"sample_index": 0, "question_index": 0},
        "results": [
            {
                "id": "mem-1",
                "text": "Alex left a blue notebook on the kitchen table.",
                "score": 0.91,
                "metadata": {
                    "memory_kind": "extracted_memory",
                    "modality": "ASSERTED",
                    "event_time": {"normalized": "2026-06-01"},
                    "evidence_refs": [
                        {
                            "message_id": "S0:D1:0",
                            "quote": "blue notebook",
                            "start_char": 10,
                            "end_char": 23,
                            "evidence_role": "support",
                        },
                        {
                            "message_id": "S0:D1:0",
                            "quote": "blue notebook",
                            "start_char": 10,
                            "end_char": 23,
                            "evidence_role": "support",
                        },
                    ],
                },
            },
            {
                "id": "mem-2",
                "text": "Jamie saw the notebook beside a mug.",
                "score": 0.8,
                "metadata": {
                    "memory_kind": "extracted_memory",
                    "modality": "ASSERTED",
                    "evidence_refs": [
                        {
                            "message_id": "S0:D1:1",
                            "quote": "next to the coffee mug",
                            "start_char": 20,
                            "end_char": 42,
                            "evidence_role": "support",
                        }
                    ],
                },
            },
        ],
    }

    contexts = render_contexts(DATASET, PREPARED, item, "extracted")

    assert len(contexts["Alex"]) == 1
    assert len(contexts["Jamie"]) == 1
    alex = contexts["Alex"][0]
    assert alex["rank"] == 1
    assert alex["memory_id"] == "mem-1"
    assert alex["evidence_id"] == "S0:D1:0"
    assert alex["timestamp"] == "9:00 am on 1 June, 2026"
    assert "[Atomic] Alex left a blue notebook" in alex["memory"]
    assert "[Modality] ASSERTED" in alex["memory"]
    assert "[Event time] 2026-06-01" in alex["memory"]
    assert "[Evidence S0:D1:0] Alex:" in alex["memory"]
    assert "before leaving for work" in alex["memory"]
    assert contexts["Jamie"][0]["rank"] == 2


def test_raw_context_uses_exact_source_turn_without_result_rewriting() -> None:
    item = {
        "query_id": "S0:Q0",
        "results": [
            {
                "id": "S0:D1:0",
                "text": "rewritten text must not be used",
                "score": 0.8,
                "metadata": {"scope_id": "locomo:S0"},
            }
        ],
    }

    contexts = render_contexts(DATASET, PREPARED, item, "raw")

    assert contexts["Alex"][0]["memory"] == (
        "Alex: I left my blue notebook on the kitchen table before leaving for work."
    )


def test_context_rejects_cross_scope_or_missing_evidence() -> None:
    cross_scope = {
        "query_id": "S0:Q0",
        "results": [
            {
                "id": "mem-cross",
                "text": "cross scope",
                "score": 0.5,
                "metadata": {
                    "evidence_refs": [{"message_id": "S1:D1:0"}]
                },
            }
        ],
    }

    with pytest.raises(ValueError, match="cross-scope evidence S1:D1:0"):
        render_contexts(DATASET, PREPARED, cross_scope, "extracted")

    missing = {
        "query_id": "S0:Q0",
        "results": [
            {
                "id": "mem-missing",
                "text": "missing",
                "score": 0.5,
                "metadata": {
                    "evidence_refs": [{"message_id": "S0:D9:9"}]
                },
            }
        ],
    }
    with pytest.raises(ValueError, match="missing source evidence S0:D9:9"):
        render_contexts(DATASET, PREPARED, missing, "extracted")

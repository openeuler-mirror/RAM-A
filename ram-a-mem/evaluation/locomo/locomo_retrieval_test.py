from __future__ import annotations

import json
from pathlib import Path

from locomo.locomo_retrieval import evaluate_prepared_query, evaluate_retrieval


FIXTURE = Path(__file__).parents[1] / "fixtures" / "locomo_sample.json"


def test_atomic_memory_with_two_refs_is_one_rank_and_covers_two_gold_turns() -> None:
    dataset = json.loads(FIXTURE.read_text(encoding="utf-8"))
    dataset[0]["qa"][0]["evidence"] = ["D1:0", "D2:0"]
    item = {
        "query_id": "S0:Q0",
        "task": {"sample_index": 0, "question_index": 0},
        "results": [
            {
                "id": "mem-1",
                "text": "combined",
                "metadata": {
                    "evidence_refs": [
                        {"message_id": "S0:D1:0"},
                        {"message_id": "S0:D2:0"},
                    ]
                },
            },
            {
                "id": "mem-2",
                "text": "other",
                "metadata": {
                    "evidence_refs": [{"message_id": "S0:D1:1"}]
                },
            },
        ],
    }

    row = evaluate_prepared_query(dataset, item, "extracted")

    assert row["retrieved_count"] == 2
    assert row["retrieved_evidence_ref_count"] == 3
    assert row["expanded_source_turn_count"] == 3
    assert row["evidence_hit"] == 1.0
    assert row["first_hit_rank"] == 1
    assert row["mrr"] == 1.0
    assert row["retrieved_evidence"] == [
        ["S0:D1:0", "S0:D2:0"],
        ["S0:D1:1"],
    ]


def test_prepared_raw_uses_one_source_turn_per_rank() -> None:
    dataset = json.loads(FIXTURE.read_text(encoding="utf-8"))
    item = {
        "query_id": "S0:Q0",
        "task": {"sample_index": 0, "question_index": 0},
        "results": [
            {"id": "S0:D1:1", "text": "miss", "metadata": {}},
            {"id": "S0:D1:0", "text": "hit", "metadata": {}},
        ],
    }

    row = evaluate_prepared_query(dataset, item, "raw")

    assert row["evidence_hit"] == 1.0
    assert row["first_hit_rank"] == 2
    assert row["mrr"] == 0.5
    assert row["retrieved_evidence_ref_count"] == 2
    assert row["expanded_source_turn_count"] == 2


def test_extracted_diagnostics_separate_evidence_spans_from_source_turns() -> None:
    dataset = json.loads(FIXTURE.read_text(encoding="utf-8"))
    item = {
        "query_id": "S0:Q0",
        "results": [
            {
                "id": "mem-1",
                "text": "atomic",
                "metadata": {
                    "evidence_refs": [
                        {
                            "message_id": "S0:D1:0",
                            "start_char": 0,
                            "end_char": 10,
                            "evidence_role": "primary",
                        },
                        {
                            "message_id": "S0:D1:0",
                            "start_char": 20,
                            "end_char": 30,
                            "evidence_role": "supporting",
                        },
                    ]
                },
            }
        ],
    }

    row = evaluate_prepared_query(dataset, item, "extracted")

    assert row["retrieved_evidence_ref_count"] == 2
    assert row["expanded_source_turn_count"] == 1
    assert row["evidence_refs_per_result"] == 2.0


def test_prepared_retrieval_summary_separates_ranks_refs_and_expansion() -> None:
    dataset = json.loads(FIXTURE.read_text(encoding="utf-8"))
    item = {
        "query_id": "S0:Q0",
        "task": {"sample_index": 0, "question_index": 0},
        "results": [
            {
                "id": "mem-1",
                "text": "atomic",
                "metadata": {
                    "evidence_refs": [
                        {"message_id": "S0:D1:0"},
                        {"message_id": "S0:D1:1"},
                    ]
                },
            }
        ],
    }

    report = evaluate_retrieval(
        dataset,
        [item],
        Path("search.json"),
        prepared_mode="extracted",
    )

    assert report["overall"]["count"] == 1
    assert report["overall"]["avg_retrieved_contexts"] == 1.0
    assert report["overall"]["avg_evidence_refs_per_result"] == 2.0
    assert report["overall"]["avg_expanded_source_turns"] == 2.0

import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from locomo.prepare_memory_bench import build_prepared_dataset
from locomo.locomo_retrieval import evaluate_query
from locomo.locomo_responses import (
    MemoryBenchResponses,
    format_context_memory,
    format_graph_fact_validity,
)


def test_build_prepared_dataset_preserves_locomo_context_for_graph_ingestion():
    raw = [
        {
            "sample_id": "locomo-sample-1",
            "conversation": {
                "speaker_a": "Alex",
                "speaker_b": "Jamie",
                "session_1_date_time": "9:00 am on 1 June, 2026",
                "session_1": [
                    {
                        "speaker": "Alex",
                        "dia_id": "D1:0",
                        "text": "I left my blue notebook on the kitchen table.",
                    }
                ],
            },
            "qa": [
                {
                    "question": "Where did Alex leave the blue notebook?",
                    "answer": "On the kitchen table.",
                    "evidence": ["D1:0"],
                    "category": 1,
                }
            ],
        }
    ]

    prepared = build_prepared_dataset(raw, source="locomo_sample.json")

    assert prepared["schema_version"] == "benchmark-prepared-v1"
    assert prepared["dataset"]["name"] == "locomo"
    assert len(prepared["memories"]) == 1
    assert len(prepared["queries"]) == 1

    memory = prepared["memories"][0]
    assert memory["id"] == "$[0].conversation.session_1[0].text:0"
    assert memory["text"] == "I left my blue notebook on the kitchen table."
    assert memory["metadata"]["scope_id"] == "path:$[0]"
    assert memory["metadata"]["raw_memory_path"] == "$[0].conversation.session_1[0].text"
    assert memory["metadata"]["speaker"] == "Alex"
    assert memory["metadata"]["graph_source_entity"] == {
        "name": "Alex",
        "entity_type": "PERSON",
    }
    assert memory["metadata"]["dia_id"] == "D1:0"
    assert memory["metadata"]["session_id"] == "session_1"
    assert memory["metadata"]["turn_index"] == 0
    assert memory["metadata"]["session_timestamp"] == "9:00 am on 1 June, 2026"
    assert memory["metadata"]["observed_at_ms"] == 1_780_304_400_000

    query = prepared["queries"][0]
    assert query["id"] == "$[0].qa[0].question"
    assert query["filter"] == {"scope_id": "path:$[0]"}
    assert query["metadata"]["raw_query_path"] == "$[0].qa[0].question"
    assert query["metadata"]["category"] == 1
    assert "target_speaker" not in query["metadata"]
    assert query["task"]["type"] == "open_qa"
    assert query["task"]["correct_answer"] == "On the kitchen table."

def test_graph_fact_validity_formats_open_and_closed_time_bounds():
    assert (
        format_graph_fact_validity(
            {"valid_from_ms": 1_780_304_400_000, "valid_to_ms": None}
        )
        == "valid from 2026-06-01T09:00:00Z"
    )
    assert (
        format_graph_fact_validity(
            {"valid_from_ms": None, "valid_to_ms": 1_780_308_000_000}
        )
        == "valid until 2026-06-01T10:00:00Z"
    )
    assert (
        format_graph_fact_validity(
            {
                "valid_from_ms": 1_780_304_400_000,
                "valid_to_ms": 1_780_304_400_000,
            }
        )
        == "valid at 2026-06-01T09:00:00Z"
    )
    assert (
        format_graph_fact_validity(
            {
                "valid_from_ms": 1_780_304_400_000,
                "valid_to_ms": 1_780_308_000_000,
            }
        )
        == "valid from 2026-06-01T09:00:00Z to 2026-06-01T10:00:00Z"
    )
    assert format_graph_fact_validity({}) is None


def test_retrieval_uses_raw_paths_from_prepared_memory_bench_output():
    dataset = json.loads(
        """
        [{
          "conversation": {
            "speaker_a": "Alex",
            "speaker_b": "Jamie",
            "session_1_date_time": "9:00 am on 1 June, 2026",
            "session_1": [{"speaker": "Alex", "dia_id": "D1:0", "text": "Notebook on table."}]
          },
          "qa": [{
            "question": "Where was the notebook?",
            "answer": "On the table.",
            "evidence": ["D1:0"],
            "category": 1
          }]
        }]
        """
    )
    query_output = {
        "query_path": "$.queries[0].text",
        "metadata": {"raw_query_path": "$[0].qa[0].question"},
        "results": [
            {
                "text": "Notebook on table.",
                "metadata": {"raw_memory_path": "$[0].conversation.session_1[0].text"},
                "score": 1.0,
            }
        ],
    }

    row = evaluate_query(dataset, query_output)

    assert row["evidence_hit"] == 1.0
    assert row["retrieved_evidence"] == ["S0:D1:0"]


def test_responses_reconstruct_context_from_prepared_raw_memory_path():
    dataset = [
        {
            "conversation": {
                "speaker_a": "Alex",
                "speaker_b": "Jamie",
                "session_1_date_time": "9:00 am on 1 June, 2026",
                "session_1": [
                    {"speaker": "Alex", "dia_id": "D1:0", "text": "Notebook on table."}
                ],
            }
        }
    ]

    contexts = MemoryBenchResponses.retrieve_context(
        dataset,
        0,
        dataset[0]["conversation"],
        [
            {
                "text": "Notebook on table.",
                "metadata": {
                    "raw_memory_path": "$[0].conversation.session_1[0].text",
                    "graph_facts": [
                        {
                            "fact_id": "fact-1",
                            "fact_text": "Alex left a notebook on the table.",
                            "predicate": "RELATED_TO",
                            "score": 0.8,
                            "valid_from_ms": 1_780_304_400_000,
                            "valid_to_ms": None,
                            "recorded_at_ms": 1_780_304_500_000,
                        }
                    ],
                },
                "score": 0.5,
            }
        ],
    )

    assert contexts["Alex"] == [
        {
            "memory": "Alex: Notebook on table.",
            "timestamp": "9:00 am on 1 June, 2026",
            "score": 0.5,
            "graph_facts": [
                {
                    "fact_id": "fact-1",
                    "fact_text": "Alex left a notebook on the table.",
                    "predicate": "RELATED_TO",
                    "score": 0.8,
                    "subject": None,
                    "object": None,
                    "valid_from_ms": 1_780_304_400_000,
                    "valid_to_ms": None,
                    "recorded_at_ms": 1_780_304_500_000,
                }
            ],
        }
    ]
    assert (
        format_context_memory(contexts["Alex"][0])
        == "9:00 am on 1 June, 2026: Alex: Notebook on table.\n"
        "Matched graph facts:\n- [RELATED_TO] Alex left a notebook on the table. "
        "[valid from 2026-06-01T09:00:00Z]"
    )
    assert format_context_memory(contexts["Alex"][0], max_graph_context_facts=0) == (
        "9:00 am on 1 June, 2026: Alex: Notebook on table."
    )

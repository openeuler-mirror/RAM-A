"""Tests for LongMemEval source-turn provenance expansion."""

import pytest

from longmemeval.provenance import (
    build_source_turn_metadata,
    retrieved_source_session_ids,
    retrieved_source_turn_ids,
)


def test_extracted_results_expand_evidence_in_rank_order_without_duplicates():
    result = {
        "results": [
            {
                "id": "mem-a",
                "metadata": {
                    "evidence_refs": [
                        {"message_id": "q_s0_t1"},
                        {"message_id": "q_s0_t2"},
                    ]
                },
            },
            {
                "id": "mem-b",
                "metadata": {
                    "evidence_refs": [
                        {"message_id": "q_s0_t2"},
                        {"message_id": "q_s1_t0"},
                    ]
                },
            },
        ]
    }

    assert retrieved_source_turn_ids(result) == [
        "q_s0_t1",
        "q_s0_t2",
        "q_s1_t0",
    ]


def test_raw_results_keep_their_record_ids():
    result = {
        "results": [
            {"id": "q_s0_t1", "metadata": {"session_id": "s0"}},
            {"id": "q_s0_t2", "metadata": {"session_id": "s0"}},
        ]
    }

    assert retrieved_source_turn_ids(result) == ["q_s0_t1", "q_s0_t2"]


def test_nested_observation_evidence_uses_the_writer_schema_as_fallback():
    result = {
        "results": [
            {
                "id": "mem-a",
                "metadata": {
                    "memory_kind": "extracted_memory",
                    "observation_refs": [
                        {
                            "source_window_id": "window-1",
                            "evidence_refs": [
                                {"message_id": "q_s0_t2"},
                                {"message_id": "q_s0_t1"},
                            ],
                        },
                        {
                            "source_window_id": "window-2",
                            "evidence_refs": [
                                {"message_id": "q_s0_t1"},
                                {"message_id": "q_s1_t0"},
                            ],
                        },
                    ],
                },
            }
        ]
    }

    assert retrieved_source_turn_ids(result) == [
        "q_s0_t2",
        "q_s0_t1",
        "q_s1_t0",
    ]


def test_source_turn_metadata_is_built_only_from_raw_prepared_memories():
    raw_prepared = {
        "memories": [
            {
                "id": "q_s0_t1",
                "metadata": {"session_id": "s0", "session_idx": 0},
            },
            {
                "id": "mem-a",
                "metadata": {
                    "memory_kind": "extracted_memory",
                    "session_id": "must-not-be-used",
                },
            },
        ]
    }

    assert build_source_turn_metadata(raw_prepared) == {
        "q_s0_t1": {"session_id": "s0", "session_idx": 0}
    }


def test_session_ids_are_recovered_from_raw_prepared():
    mapping = {
        "q_s0_t1": {"session_id": "s0"},
        "q_s1_t0": {"session_id": "s1"},
    }
    result = {
        "results": [
            {
                "id": "mem",
                "metadata": {
                    "evidence_refs": [
                        {"message_id": "q_s1_t0"},
                        {"message_id": "q_s0_t1"},
                    ]
                },
            }
        ]
    }

    assert retrieved_source_session_ids(result, mapping) == ["s1", "s0"]


def test_session_scoring_rejects_missing_raw_source_metadata():
    result = {
        "results": [
            {
                "id": "mem",
                "metadata": {
                    "evidence_refs": [{"message_id": "q_s1_t0"}]
                },
            }
        ]
    }

    with pytest.raises(ValueError, match="missing source turn metadata.*q_s1_t0"):
        retrieved_source_session_ids(result, {})

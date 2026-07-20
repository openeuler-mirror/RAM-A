from __future__ import annotations

import json
from pathlib import Path

import pytest

from common.memory_pipeline.extraction import ExtractionBatch, ModelUsage
from common.memory_pipeline.grounding import StaticGroundingVerifier
from common.memory_pipeline.pipeline import PipelineConfig, run_memory_pipeline
from locomo.locomo_adapter import prepare_locomo, source_lookup


FIXTURE = Path(__file__).parents[1] / "fixtures" / "locomo_sample.json"


class CapturingExtractor:
    model = "fixture"
    prompt_version = "capture_v1"

    def __init__(self) -> None:
        self.seen: list[str] = []

    def extract(self, window, messages_by_id) -> ExtractionBatch:
        payload = [
            messages_by_id[message_id].to_dict()
            for message_id in window.candidate_message_ids
        ]
        self.seen.append(json.dumps(payload, sort_keys=True))
        return ExtractionBatch(
            window_id=window.id,
            schema_version="atomic_memory_v1",
            raw_memories=[],
            usage=ModelUsage(),
            raw_response="{}",
        )


def test_prepare_locomo_preserves_turn_and_query_provenance() -> None:
    dataset = json.loads(FIXTURE.read_text(encoding="utf-8"))

    prepared = prepare_locomo(dataset)

    assert prepared["schema_version"] == "benchmark-prepared-v1"
    assert prepared["memories"][0] == {
        "id": "S0:D1:0",
        "text": "I left my blue notebook on the kitchen table before leaving for work.",
        "metadata": {
            "memory_kind": "raw_turn",
            "scope_id": "locomo:S0",
            "session_id": "S0:session_1",
            "sample_index": 0,
            "session_number": 1,
            "turn_index": 0,
            "dia_id": "D1:0",
            "speaker": "Alex",
            "role": "speaker_a",
            "timestamp": "9:00 am on 1 June, 2026",
        },
    }
    assert prepared["queries"][0] == {
        "id": "S0:Q0",
        "text": "Where did Alex leave the blue notebook?",
        "filter": {"scope_id": "locomo:S0"},
        "metadata": {"sample_index": 0, "question_index": 0},
        "task": {
            "sample_index": 0,
            "question_index": 0,
            "category": 1,
            "answer": "On the kitchen table.",
            "evidence_ids": ["S0:D1:0"],
        },
    }


def test_subset_keeps_original_indexes_and_source_lookup_is_deterministic() -> None:
    dataset = json.loads(FIXTURE.read_text(encoding="utf-8"))

    first = prepare_locomo(dataset, sample_indexes=(0,))
    second = prepare_locomo(dataset, sample_indexes=(0,))

    assert first == second
    assert {query["id"] for query in first["queries"]} == {
        "S0:Q0",
        "S0:Q1",
        "S0:Q2",
    }
    assert source_lookup(first)["S0:D2:0"]["metadata"]["speaker"] == "Alex"


def test_prepare_locomo_rejects_unknown_sample_index() -> None:
    dataset = json.loads(FIXTURE.read_text(encoding="utf-8"))

    with pytest.raises(ValueError, match="sample index out of range: 1"):
        prepare_locomo(dataset, sample_indexes=(1,))


def test_prepare_locomo_allows_category_five_without_gold_answer() -> None:
    dataset = json.loads(FIXTURE.read_text(encoding="utf-8"))
    dataset[0]["qa"][2].pop("answer")
    dataset[0]["qa"][2]["adversarial_answer"] = "hallucinated train"

    prepared = prepare_locomo(dataset)

    assert prepared["queries"][2]["task"]["category"] == 5
    assert prepared["queries"][2]["task"]["answer"] == ""


def test_source_lookup_rejects_duplicate_memory_ids() -> None:
    dataset = json.loads(FIXTURE.read_text(encoding="utf-8"))
    prepared = prepare_locomo(dataset)
    prepared["memories"].append(dict(prepared["memories"][0]))

    with pytest.raises(ValueError, match="duplicate source memory id: S0:D1:0"):
        source_lookup(prepared)


def test_locomo_gold_fields_never_enter_extraction_input() -> None:
    prepared = prepare_locomo(json.loads(FIXTURE.read_text(encoding="utf-8")))
    prepared["queries"][0]["text"] = "QUERY_CANARY"
    prepared["queries"][0]["task"]["answer"] = "ANSWER_CANARY"
    prepared["queries"][0]["task"]["evidence_ids"] = ["EVIDENCE_CANARY"]
    original_queries = json.loads(json.dumps(prepared["queries"]))
    extractor = CapturingExtractor()

    run = run_memory_pipeline(
        prepared,
        PipelineConfig(),
        extractor,
        StaticGroundingVerifier({}),
    )

    joined = "\n".join(extractor.seen)
    assert "QUERY_CANARY" not in joined
    assert "ANSWER_CANARY" not in joined
    assert "EVIDENCE_CANARY" not in joined
    assert run.prepared["queries"] == original_queries

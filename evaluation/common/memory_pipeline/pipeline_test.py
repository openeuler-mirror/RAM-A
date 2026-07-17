from __future__ import annotations

import copy
import json
from dataclasses import replace

import pytest

from common.memory_pipeline.cache import JsonCache
from common.memory_pipeline.episode import EpisodeConfig
from common.memory_pipeline.extraction import ExtractionBatch, ModelUsage
from common.memory_pipeline.grounding import GroundingBatch, GroundingResult
from common.memory_pipeline.pipeline import (
    PipelineConfig,
    _candidate_span_metrics,
    run_memory_pipeline,
    write_pipeline_artifacts,
)
from common.memory_pipeline.validation import ValidationConfig
from common.memory_pipeline.window import WindowConfig


RAW_PREPARED = {
    "schema_version": "benchmark-prepared-v1",
    "dataset": {"name": "fixture", "split": "test"},
    "memories": [
        {
            "id": "m1",
            "text": "I plan to move to Hangzhou in August.",
            "metadata": {
                "scope_id": "u1",
                "session_id": "s1",
                "role": "user",
                "speaker": "Alice",
                "timestamp": "2026-07-14T10:00:00Z",
            },
        }
    ],
    "queries": [
        {"id": "q1", "text": "Where will I move?", "filter": {"scope_id": "u1"}}
    ],
}


class _FixtureExtractor:
    model = "fixture-extractor"
    prompt_version = "extract_v1"

    def __init__(self, empty: bool = False) -> None:
        self.empty = empty
        self.calls = 0

    def extract(self, window, messages_by_id):
        del messages_by_id
        self.calls += 1
        memories = []
        if not self.empty:
            memories = [
                {
                    "text": "Alice plans to move to Hangzhou in August 2026.",
                    "memory_type": "event",
                    "subject": {"name": "Alice", "source_speaker": "Alice"},
                    "predicate": "plans_to_move_to",
                    "object": {"name": "Hangzhou", "type": "place"},
                    "modality": "planned",
                    "event_time": {
                        "raw": "in August",
                        "normalized": "2026-08",
                        "precision": "month",
                    },
                    "attributes": {},
                    "evidence": [
                        {
                            "message_id": "m1",
                            "quote": "plan to move to Hangzhou in August",
                            "evidence_role": "primary",
                        }
                    ],
                    "model_confidence": 0.99,
                }
            ]
        return ExtractionBatch(
            window_id=window.id,
            schema_version="atomic_memory_v1",
            raw_memories=memories,
            usage=ModelUsage(latency_ms=12.0, total_tokens=20),
        )


class _SupportingVerifier:
    model = "fixture-verifier"
    prompt_version = "ground_v1"

    def __init__(self, status: str = "SUPPORTED") -> None:
        self.status = status
        self.calls = 0

    def verify(self, window, memories, messages_by_id):
        del messages_by_id
        self.calls += 1
        return GroundingBatch(
            window_id=window.id,
            results=[GroundingResult(memory.id, self.status, "fixture") for memory in memories],
            usage=ModelUsage(latency_ms=8.0, total_tokens=10),
        )


def _config() -> PipelineConfig:
    return PipelineConfig(
        episode=EpisodeConfig(),
        window=WindowConfig(
            max_candidate_tokens=64,
            max_window_tokens=128,
            context_before_messages=2,
        ),
        validation=ValidationConfig(),
        pipeline_version="memory_pipeline_v1",
    )


def test_pipeline_builds_supported_memory_and_artifacts(tmp_path) -> None:
    run = run_memory_pipeline(
        RAW_PREPARED,
        _config(),
        _FixtureExtractor(),
        _SupportingVerifier(),
    )

    assert len(run.prepared["memories"]) == 1
    record = run.prepared["memories"][0]
    assert record["metadata"]["memory_kind"] == "extracted_memory"
    assert record["metadata"]["evidence_refs"][0]["message_id"] == "m1"
    assert run.prepared["queries"] == RAW_PREPARED["queries"]
    assert run.stats["candidate_source_coverage"] == 1.0
    assert run.stats["candidate_source_duplication"] == 0
    assert run.stats["extraction_total_tokens"] == 20
    assert run.stats["verification_total_tokens"] == 10

    write_pipeline_artifacts(run, tmp_path)

    expected = {
        "normalized_messages.jsonl",
        "episodes.jsonl",
        "extraction_windows.jsonl",
        "extracted_candidates.jsonl",
        "accepted_memories.jsonl",
        "rejected_extractions.jsonl",
        "quarantined_memories.jsonl",
        "extraction_stats.json",
        "run_metadata.json",
        "prepared.json",
    }
    assert {path.name for path in tmp_path.iterdir()} == expected
    prepared = json.loads((tmp_path / "prepared.json").read_text(encoding="utf-8"))
    assert prepared == run.prepared


def test_pipeline_reports_grounding_calls_latency_and_source_counts() -> None:
    run = run_memory_pipeline(
        RAW_PREPARED,
        _config(),
        _FixtureExtractor(),
        _SupportingVerifier(),
    )

    assert run.stats["extraction_call_count"] == 1
    assert run.stats["verification_call_count"] == 1
    assert run.stats["extraction_latency_ms"] == 12.0
    assert run.stats["verification_latency_ms"] == 8.0
    assert run.stats["grounding_status_counts"] == {"SUPPORTED": 1}
    assert run.stats["source_turn_memory_counts"] == {"m1": 1}
    assert run.stats["source_turn_evidence_ref_counts"] == {"m1": 1}


def test_pipeline_source_count_maps_include_unreferenced_turns_as_zero() -> None:
    prepared = copy.deepcopy(RAW_PREPARED)
    prepared["memories"].append(
        {
            "id": "m2",
            "text": "No durable fact here.",
            "metadata": {
                "scope_id": "u1",
                "session_id": "s1",
                "role": "assistant",
                "speaker": "Bob",
                "timestamp": "2026-07-14T10:01:00Z",
            },
        }
    )

    run = run_memory_pipeline(
        prepared,
        _config(),
        _FixtureExtractor(),
        _SupportingVerifier(),
    )

    assert run.stats["source_turn_memory_counts"] == {"m1": 1, "m2": 0}
    assert run.stats["source_turn_evidence_ref_counts"] == {"m1": 1, "m2": 0}


def test_empty_extraction_is_success_not_rejection() -> None:
    verifier = _SupportingVerifier()

    run = run_memory_pipeline(
        RAW_PREPARED,
        _config(),
        _FixtureExtractor(empty=True),
        verifier,
    )

    assert run.prepared["memories"] == []
    assert run.rejected == []
    assert run.quarantined == []
    assert run.stats["empty_extraction_windows"] == 1
    assert verifier.calls == 0


def test_non_supported_grounding_is_quarantined() -> None:
    run = run_memory_pipeline(
        RAW_PREPARED,
        _config(),
        _FixtureExtractor(),
        _SupportingVerifier(status="UNCERTAIN"),
    )

    assert run.prepared["memories"] == []
    assert run.quarantined[0].code == "grounding_uncertain"


def test_pipeline_output_is_deterministic() -> None:
    first = run_memory_pipeline(
        RAW_PREPARED,
        _config(),
        _FixtureExtractor(),
        _SupportingVerifier(),
    )
    second = run_memory_pipeline(
        RAW_PREPARED,
        _config(),
        _FixtureExtractor(),
        _SupportingVerifier(),
    )

    assert first.prepared == second.prepared
    assert first.stats == second.stats
    assert [item.to_dict() for item in first.windows] == [
        item.to_dict() for item in second.windows
    ]


def test_candidate_duplication_metric_detects_repeated_span() -> None:
    run = run_memory_pipeline(
        RAW_PREPARED,
        _config(),
        _FixtureExtractor(empty=True),
        _SupportingVerifier(),
    )
    window = run.windows[0]
    duplicated = replace(
        window,
        candidate_refs=window.candidate_refs + window.candidate_refs,
    )

    coverage, duplication = _candidate_span_metrics(
        run.normalized_messages,
        [duplicated],
    )

    assert coverage == 1.0
    assert duplication == len(RAW_PREPARED["memories"][0]["text"])


def test_pipeline_reuses_extraction_and_grounding_cache(tmp_path) -> None:
    extractor = _FixtureExtractor()
    verifier = _SupportingVerifier()
    cache = JsonCache(tmp_path / "cache")

    first = run_memory_pipeline(
        RAW_PREPARED, _config(), extractor, verifier, cache=cache
    )
    second = run_memory_pipeline(
        RAW_PREPARED, _config(), extractor, verifier, cache=cache
    )

    assert extractor.calls == 1
    assert verifier.calls == 1
    assert first.prepared == second.prepared
    assert second.stats["extraction_cache_hits"] == 1
    assert second.stats["verification_cache_hits"] == 1
    assert second.stats["extraction_total_tokens"] == 0
    assert second.stats["verification_total_tokens"] == 0


def test_extraction_cache_key_covers_rendered_message_metadata(tmp_path) -> None:
    extractor = _FixtureExtractor()
    verifier = _SupportingVerifier()
    cache = JsonCache(tmp_path / "cache")
    changed = copy.deepcopy(RAW_PREPARED)
    changed["memories"][0]["metadata"]["speaker"] = "Alicia"

    run_memory_pipeline(
        RAW_PREPARED, _config(), extractor, verifier, cache=cache
    )
    run_memory_pipeline(
        changed, _config(), extractor, verifier, cache=cache
    )

    assert extractor.calls == 2
    assert verifier.calls == 2


def test_run_metadata_identifies_source_schema_and_cache(tmp_path) -> None:
    run = run_memory_pipeline(
        RAW_PREPARED,
        _config(),
        _FixtureExtractor(),
        _SupportingVerifier(),
        cache=JsonCache(tmp_path, version="fixture_cache_v2"),
    )

    assert run.run_metadata["dataset"] == RAW_PREPARED["dataset"]
    assert run.run_metadata["source_hash"]
    assert run.run_metadata["normalizer_version"] == "normalize_v1"
    assert run.run_metadata["extractor"]["schema_version"] == "atomic_memory_v1"
    assert run.run_metadata["cache_version"] == "fixture_cache_v2"


class _FailingExtractor(_FixtureExtractor):
    def extract(self, window, messages_by_id):
        del window, messages_by_id
        raise RuntimeError("fixture extraction failed")


def test_pipeline_fail_fast_controls_model_errors() -> None:
    with pytest.raises(RuntimeError, match="fixture extraction failed"):
        run_memory_pipeline(
            RAW_PREPARED,
            _config(),
            _FailingExtractor(),
            _SupportingVerifier(),
        )

    run = run_memory_pipeline(
        RAW_PREPARED,
        replace(_config(), fail_fast=False),
        _FailingExtractor(),
        _SupportingVerifier(),
    )

    assert run.prepared["memories"] == []
    assert run.rejected[0].code == "extract_runtime_error"

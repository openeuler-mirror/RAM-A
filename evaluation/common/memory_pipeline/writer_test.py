from __future__ import annotations

import copy

from common.memory_pipeline.models import AtomicMemory, EvidenceRef
from common.memory_pipeline.writer import aggregate_exact_memories, make_prepared_output


def _evidence(message_id: str, start: int = 0) -> EvidenceRef:
    return EvidenceRef(
        message_id=message_id,
        quote="source quote",
        start_char=start,
        end_char=start + 12,
        evidence_role="primary",
    )


def _memory(
    text: str,
    evidence: tuple[EvidenceRef, ...],
    *,
    window_id: str,
    observed_at: str,
) -> AtomicMemory:
    return AtomicMemory(
        id="candidate-id",
        scope_id="u1",
        text=text,
        memory_type="state",
        subject={"name": "user"},
        predicate="lives_in",
        object={"name": "Hangzhou", "type": "place"},
        modality="asserted",
        evidence=evidence,
        event_time={"normalized": "2026-07", "precision": "month"},
        observed_at=observed_at,
        source_episode_id="episode-1",
        source_window_id=window_id,
    )


def test_exact_duplicate_aggregates_evidence_without_semantic_merge() -> None:
    first = _memory(
        "User lives in Hangzhou.",
        (_evidence("m1"),),
        window_id="w1",
        observed_at="2026-07-01T10:00:00Z",
    )
    second = _memory(
        "User lives in Hangzhou.",
        (_evidence("m2"),),
        window_id="w2",
        observed_at="2026-07-02T10:00:00Z",
    )
    paraphrase = _memory(
        "The user lives in Hangzhou.",
        (_evidence("m3"),),
        window_id="w3",
        observed_at="2026-07-03T10:00:00Z",
    )

    result = aggregate_exact_memories([first, second, paraphrase])

    assert len(result) == 2
    exact = next(item for item in result if item.text == "User lives in Hangzhou.")
    assert {item.message_id for item in exact.evidence} == {"m1", "m2"}
    assert [item["source_window_id"] for item in exact.observation_refs] == ["w1", "w2"]
    assert exact.id.startswith("mem-")
    assert exact.observed_at == "2026-07-02T10:00:00Z"


def test_different_scope_or_event_time_does_not_deduplicate() -> None:
    first = _memory(
        "User lives in Hangzhou.",
        (_evidence("m1"),),
        window_id="w1",
        observed_at="2026-07-01T10:00:00Z",
    )
    different_scope = AtomicMemory(**{**first.__dict__, "scope_id": "u2"})
    different_time = AtomicMemory(
        **{
            **first.__dict__,
            "event_time": {"normalized": "2025-07", "precision": "month"},
        }
    )

    assert len(aggregate_exact_memories([first, different_scope, different_time])) == 3


def test_prepared_mapping_preserves_queries_and_uses_extracted_kind() -> None:
    source = {
        "schema_version": "benchmark-prepared-v1",
        "dataset": {"name": "fixture", "split": "test"},
        "memories": [{"id": "raw", "text": "raw", "metadata": {"scope_id": "u1"}}],
        "queries": [{"id": "q1", "text": "Where?", "filter": {"scope_id": "u1"}}],
    }
    original = copy.deepcopy(source)
    memory = aggregate_exact_memories(
        [
            _memory(
                "User lives in Hangzhou.",
                (_evidence("m1"),),
                window_id="w1",
                observed_at="2026-07-01T10:00:00Z",
            )
        ]
    )[0]

    output = make_prepared_output(source, [memory], {"pipeline_version": "memory_v1"})

    assert source == original
    assert output["queries"] == source["queries"]
    assert output["dataset"] == source["dataset"]
    assert output["memory_pipeline"]["pipeline_version"] == "memory_v1"
    assert len(output["memories"]) == 1
    record = output["memories"][0]
    assert record["id"] == memory.id
    assert record["text"] == memory.text
    assert record["metadata"]["memory_kind"] == "extracted_memory"
    assert record["metadata"]["scope_id"] == "u1"
    assert record["metadata"]["evidence_refs"][0]["message_id"] == "m1"
    assert record["metadata"]["observation_refs"][0]["source_window_id"] == "w1"

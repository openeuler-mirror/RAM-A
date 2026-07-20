"""Exact aggregation and RAM-A prepared-record mapping."""

from __future__ import annotations

import copy
from dataclasses import replace
from typing import Any, Sequence

from .canonical import canonical_json, stable_hash
from .models import AtomicMemory, EvidenceRef


def aggregate_exact_memories(
    memories: Sequence[AtomicMemory],
) -> list[AtomicMemory]:
    grouped: dict[str, AtomicMemory] = {}
    evidence_by_key: dict[str, list[EvidenceRef]] = {}
    observations_by_key: dict[str, list[dict[str, Any]]] = {}

    for memory in memories:
        key = _memory_key(memory)
        if key not in grouped:
            grouped[key] = memory
            evidence_by_key[key] = []
            observations_by_key[key] = []
        _extend_unique_evidence(evidence_by_key[key], memory.evidence)
        _extend_unique_observations(
            observations_by_key[key],
            memory.observation_refs or (_observation(memory),),
        )

    output: list[AtomicMemory] = []
    for key, memory in grouped.items():
        observations = observations_by_key[key]
        observed_values = [str(item.get("observed_at") or "") for item in observations]
        latest_observed_at = max(observed_values, default="")
        output.append(
            replace(
                memory,
                id="mem-" + stable_hash(memory.scope_id, memory.canonical_content()),
                evidence=tuple(evidence_by_key[key]),
                observed_at=latest_observed_at,
                observation_refs=tuple(observations),
            )
        )
    return output


def make_prepared_output(
    source_prepared: dict[str, Any],
    memories: Sequence[AtomicMemory],
    run_metadata: dict[str, Any],
) -> dict[str, Any]:
    if source_prepared.get("schema_version") != "benchmark-prepared-v1":
        raise ValueError("source prepared input must use benchmark-prepared-v1")
    return {
        "schema_version": "benchmark-prepared-v1",
        "dataset": copy.deepcopy(source_prepared.get("dataset", {})),
        "memory_pipeline": copy.deepcopy(run_metadata),
        "memories": [_memory_record(memory) for memory in memories],
        "queries": copy.deepcopy(source_prepared.get("queries", [])),
    }


def _memory_key(memory: AtomicMemory) -> str:
    return canonical_json(
        {
            "scope_id": memory.scope_id,
            "content": memory.canonical_content(),
        }
    )


def _observation(memory: AtomicMemory) -> dict[str, Any]:
    return {
        "source_episode_id": memory.source_episode_id,
        "source_window_id": memory.source_window_id,
        "observed_at": memory.observed_at,
        "evidence_refs": [item.to_dict() for item in memory.evidence],
    }


def _extend_unique_evidence(
    target: list[EvidenceRef],
    incoming: Sequence[EvidenceRef],
) -> None:
    seen = {
        (
            item.message_id,
            item.start_char,
            item.end_char,
            item.evidence_role,
        )
        for item in target
    }
    for item in incoming:
        key = (
            item.message_id,
            item.start_char,
            item.end_char,
            item.evidence_role,
        )
        if key not in seen:
            target.append(item)
            seen.add(key)


def _extend_unique_observations(
    target: list[dict[str, Any]],
    incoming: Sequence[dict[str, Any]],
) -> None:
    seen = {canonical_json(item) for item in target}
    for item in incoming:
        canonical = canonical_json(item)
        if canonical not in seen:
            target.append(copy.deepcopy(item))
            seen.add(canonical)


def _memory_record(memory: AtomicMemory) -> dict[str, Any]:
    metadata = {
        "schema_version": "atomic_memory_v1",
        "memory_kind": "extracted_memory",
        "memory_type": memory.memory_type,
        "scope_id": memory.scope_id,
        "subject": copy.deepcopy(memory.subject),
        "predicate": memory.predicate,
        "object": copy.deepcopy(memory.object),
        "modality": memory.modality,
        "event_time": copy.deepcopy(memory.event_time),
        "attributes": copy.deepcopy(memory.attributes),
        "observed_at": memory.observed_at,
        "source_episode_id": memory.source_episode_id,
        "source_window_id": memory.source_window_id,
        "evidence_refs": [item.to_dict() for item in memory.evidence],
        "observation_refs": [copy.deepcopy(item) for item in memory.observation_refs],
    }
    if memory.model_confidence is not None:
        metadata["model_confidence"] = memory.model_confidence
    return {
        "id": memory.id,
        "text": memory.text,
        "metadata": metadata,
    }

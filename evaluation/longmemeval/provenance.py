"""Resolve LongMemEval retrieval results back to raw source turns."""

from __future__ import annotations

from typing import Any


def build_source_turn_metadata(
    raw_prepared: dict[str, Any],
) -> dict[str, dict[str, Any]]:
    """Index raw prepared-memory metadata by source turn ID."""
    source_turn_metadata: dict[str, dict[str, Any]] = {}
    for memory in raw_prepared.get("memories", []):
        metadata = memory.get("metadata") or {}
        if metadata.get("memory_kind") == "extracted_memory":
            continue
        memory_id = str(memory.get("id") or "")
        if memory_id:
            source_turn_metadata[memory_id] = dict(metadata)
    return source_turn_metadata


def retrieved_source_turn_ids(result: dict[str, Any]) -> list[str]:
    """Expand ranked results to unique raw source turn IDs in order."""
    seen: set[str] = set()
    ordered: list[str] = []

    for item in result.get("results", []):
        metadata = item.get("metadata") or {}
        direct_ids = _message_ids(metadata.get("evidence_refs"))
        nested_ids = _observation_message_ids(metadata.get("observation_refs"))
        if (
            metadata.get("memory_kind") == "extracted_memory"
            or direct_ids
            or nested_ids
        ):
            candidates = direct_ids or nested_ids
        else:
            item_id = str(item.get("id") or "")
            candidates = [item_id] if item_id else []

        for message_id in candidates:
            if message_id not in seen:
                seen.add(message_id)
                ordered.append(message_id)

    return ordered


def retrieved_source_session_ids(
    result: dict[str, Any],
    source_turn_metadata: dict[str, dict[str, Any]],
) -> list[str]:
    """Recover unique sessions only through raw source-turn metadata."""
    seen: set[str] = set()
    ordered: list[str] = []
    for turn_id in retrieved_source_turn_ids(result):
        if turn_id not in source_turn_metadata:
            raise ValueError(f"missing source turn metadata for {turn_id!r}")
        session_id = str(source_turn_metadata[turn_id].get("session_id") or "")
        if not session_id:
            raise ValueError(f"source turn metadata for {turn_id!r} has no session_id")
        if session_id not in seen:
            seen.add(session_id)
            ordered.append(session_id)
    return ordered


def _message_ids(refs: Any) -> list[str]:
    if not isinstance(refs, list):
        return []
    return [
        message_id
        for ref in refs
        if isinstance(ref, dict)
        if (message_id := str(ref.get("message_id") or ""))
    ]


def _observation_message_ids(observations: Any) -> list[str]:
    if not isinstance(observations, list):
        return []
    message_ids: list[str] = []
    for observation in observations:
        if isinstance(observation, dict):
            message_ids.extend(_message_ids(observation.get("evidence_refs")))
    return message_ids

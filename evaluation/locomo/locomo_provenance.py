"""Resolve LoCoMo query and source provenance for prepared memory results."""

from __future__ import annotations

from dataclasses import dataclass
import json
import re
from typing import Any

from locomo.locomo_adapter import source_lookup


QUERY_ID_RE = re.compile(r"^S(\d+):Q(\d+)$")
SOURCE_ID_RE = re.compile(r"^S(\d+):D\d+:\d+$")


@dataclass(frozen=True)
class QueryRef:
    sample_index: int
    question_index: int


def query_ref(item: dict[str, Any]) -> QueryRef:
    """Resolve a stable prepared query reference."""

    match = QUERY_ID_RE.fullmatch(str(item.get("query_id") or ""))
    if match:
        return QueryRef(*(int(value) for value in match.groups()))
    task = item.get("task") or {}
    try:
        return QueryRef(
            sample_index=int(task["sample_index"]),
            question_index=int(task["question_index"]),
        )
    except (KeyError, TypeError, ValueError) as error:
        raise ValueError(
            f"search output has no supported query reference: {item.get('query_id')!r}"
        ) from error


def result_evidence_ids(
    result: dict[str, Any],
    mode: str,
) -> tuple[str, ...]:
    """Return evidence IDs for one retrieval rank without expanding its rank."""

    if mode == "raw":
        memory_id = str(result.get("id") or "")
        return (memory_id,) if memory_id else ()
    if mode != "extracted":
        raise ValueError(f"unsupported memory mode: {mode}")
    refs = (result.get("metadata") or {}).get("evidence_refs") or []
    return tuple(
        dict.fromkeys(
            str(ref.get("message_id") or "")
            for ref in refs
            if str(ref.get("message_id") or "")
        )
    )


def render_contexts(
    dataset: list[dict[str, Any]],
    source_prepared: dict[str, Any],
    item: dict[str, Any],
    mode: str,
) -> dict[str, list[dict[str, Any]]]:
    """Render raw turns or atomic claims plus their exact source turns."""

    ref = query_ref(item)
    try:
        conversation = dataset[ref.sample_index]["conversation"]
    except (IndexError, KeyError, TypeError) as error:
        raise ValueError(f"invalid LoCoMo query reference: {ref}") from error
    speaker_a = str(conversation["speaker_a"])
    speaker_b = str(conversation["speaker_b"])
    contexts: dict[str, list[dict[str, Any]]] = {
        speaker_a: [],
        speaker_b: [],
    }
    sources = source_lookup(source_prepared)

    for rank, result in enumerate(item.get("results", []), start=1):
        evidence_ids = result_evidence_ids(result, mode)
        if mode == "extracted" and not evidence_ids:
            raise ValueError(
                f"extracted result {result.get('id')!r} has no evidence refs"
            )
        refs_by_id = _refs_by_message_id(result)
        for evidence_id in evidence_ids:
            _validate_scope(evidence_id, ref.sample_index)
            try:
                source = sources[evidence_id]
            except KeyError as error:
                raise ValueError(
                    f"missing source evidence {evidence_id} for result {result.get('id')!r}"
                ) from error
            metadata = source.get("metadata") or {}
            speaker = str(metadata.get("speaker") or "")
            if speaker not in contexts:
                raise ValueError(
                    f"source evidence {evidence_id} has unknown speaker {speaker!r}"
                )
            evidence_ref = refs_by_id.get(evidence_id, {})
            contexts[speaker].append(
                {
                    "memory": _render_memory(result, source, evidence_id, mode),
                    "timestamp": str(metadata.get("timestamp") or ""),
                    "score": float(result.get("score") or 0.0),
                    "rank": rank,
                    "memory_id": str(result.get("id") or ""),
                    "evidence_id": evidence_id,
                    "quote": str(evidence_ref.get("quote") or ""),
                    "graph_facts": (result.get("metadata") or {}).get("graph_facts"),
                }
            )
    return contexts


def _refs_by_message_id(result: dict[str, Any]) -> dict[str, dict[str, Any]]:
    refs = (result.get("metadata") or {}).get("evidence_refs") or []
    unique: dict[str, dict[str, Any]] = {}
    for ref in refs:
        message_id = str(ref.get("message_id") or "")
        if message_id and message_id not in unique:
            unique[message_id] = ref
    return unique


def _validate_scope(evidence_id: str, sample_index: int) -> None:
    match = SOURCE_ID_RE.fullmatch(evidence_id)
    if match and int(match.group(1)) != sample_index:
        raise ValueError(
            f"cross-scope evidence {evidence_id} for query sample S{sample_index}"
        )


def _render_memory(
    result: dict[str, Any],
    source: dict[str, Any],
    evidence_id: str,
    mode: str,
) -> str:
    source_metadata = source.get("metadata") or {}
    speaker = str(source_metadata.get("speaker") or "Unknown")
    source_text = str(source.get("text") or "")
    if mode == "raw":
        return f"{speaker}: {source_text}"

    metadata = result.get("metadata") or {}
    lines = [f"[Atomic] {result.get('text', '')}"]
    modality = str(metadata.get("modality") or "")
    if modality:
        lines.append(f"[Modality] {modality}")
    event_time = metadata.get("event_time")
    if event_time:
        if isinstance(event_time, dict):
            rendered_time = (
                event_time.get("normalized")
                or event_time.get("raw")
                or json.dumps(event_time, ensure_ascii=False, sort_keys=True)
            )
        else:
            rendered_time = str(event_time)
        lines.append(f"[Event time] {rendered_time}")
    lines.append(f"[Evidence {evidence_id}] {speaker}: {source_text}")
    return "\n".join(lines)

"""Convert benchmark-prepared records into immutable source messages."""

from __future__ import annotations

from typing import Any

from .models import NormalizedMessage, PipelineIssue


NORMALIZER_VERSION = "normalize_v1"


def normalize_prepared_memories(
    prepared: dict[str, Any],
) -> tuple[list[NormalizedMessage], list[PipelineIssue]]:
    if prepared.get("schema_version") != "benchmark-prepared-v1":
        raise ValueError("prepared input must use benchmark-prepared-v1")

    memories = prepared.get("memories", [])
    if not isinstance(memories, list):
        raise ValueError("prepared memories must be a list")

    messages: list[NormalizedMessage] = []
    issues: list[PipelineIssue] = []
    seen_ids: set[str] = set()

    for source_index, record in enumerate(memories):
        if not isinstance(record, dict):
            issues.append(_issue("invalid_source_record", "source memory must be an object"))
            continue

        source_id = str(record.get("id") or "").strip()
        if not source_id:
            issues.append(_issue("missing_source_id", "source memory is missing id"))
            continue
        if source_id in seen_ids:
            raise ValueError(f"duplicate source message id: {source_id}")
        seen_ids.add(source_id)

        metadata = record.get("metadata") or {}
        if not isinstance(metadata, dict):
            issues.append(
                _issue(
                    "invalid_source_metadata",
                    "source memory metadata must be an object",
                    source_id,
                )
            )
            continue

        scope_id = str(metadata.get("scope_id") or "").strip()
        if not scope_id:
            issues.append(
                _issue(
                    "missing_scope_id",
                    "source memory is missing metadata.scope_id",
                    source_id,
                )
            )
            continue

        text = str(record.get("text") or "")
        if not text.strip():
            issues.append(
                _issue(
                    "blank_source_message",
                    "source memory text is blank",
                    source_id,
                    scope_id,
                )
            )
            continue

        turn_value = metadata.get("turn_index", metadata.get("turn_idx"))
        turn_index = _optional_int(turn_value)
        timestamp = _first_text(metadata, "timestamp", "created_at", "session_date")
        role = str(metadata.get("role") or "other").strip() or "other"

        messages.append(
            NormalizedMessage(
                id=source_id,
                scope_id=scope_id,
                text=text,
                role=role,
                speaker=str(metadata.get("speaker") or "").strip(),
                timestamp=timestamp,
                session_id=str(metadata.get("session_id") or "").strip(),
                turn_index=turn_index,
                source_index=source_index,
                metadata=dict(metadata),
            )
        )

    return messages, issues


def _issue(
    code: str,
    message: str,
    source_id: str = "",
    scope_id: str = "",
) -> PipelineIssue:
    return PipelineIssue(
        stage="normalize",
        code=code,
        message=message,
        source_id=source_id,
        scope_id=scope_id,
    )


def _optional_int(value: Any) -> int | None:
    if value is None or value == "":
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def _first_text(metadata: dict[str, Any], *keys: str) -> str:
    for key in keys:
        value = metadata.get(key)
        if value not in (None, ""):
            return str(value).strip()
    return ""

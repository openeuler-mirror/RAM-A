"""Schema, evidence, and deterministic guards for extracted memories."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Mapping, Sequence

from .canonical import stable_hash
from .models import (
    AtomicMemory,
    EvidenceRef,
    ExtractionWindow,
    MessageRef,
    NormalizedMessage,
    PipelineIssue,
)


MEMORY_TYPES = {
    "fact",
    "preference",
    "relationship",
    "event",
    "state",
    "procedure",
    "other",
}
MODALITIES = {
    "asserted",
    "negated",
    "possible",
    "planned",
    "conditional",
    "reported",
}
EVIDENCE_ROLES = {"primary", "supporting"}
_PLAN_MARKERS = (" plan ", "plan to", "intend to", "打算", "计划", "准备")
_POSSIBLE_MARKERS = ("might", "maybe", "possibly", "可能", "也许", "或许")
_NEGATION_MARKERS = (" don't ", " not ", "never", "不", "没", "从不")


@dataclass(frozen=True)
class ValidationConfig:
    max_memory_chars: int = 500

    def __post_init__(self) -> None:
        if self.max_memory_chars <= 0:
            raise ValueError("max_memory_chars must be positive")


@dataclass(frozen=True)
class ValidationBatch:
    valid: list[AtomicMemory]
    rejected: list[PipelineIssue]
    quarantined: list[PipelineIssue]


def validate_extraction(
    raw_memories: Sequence[dict[str, Any]],
    window: ExtractionWindow,
    messages_by_id: Mapping[str, NormalizedMessage],
    config: ValidationConfig,
) -> ValidationBatch:
    valid: list[AtomicMemory] = []
    rejected: list[PipelineIssue] = []
    quarantined: list[PipelineIssue] = []

    for index, raw in enumerate(raw_memories):
        schema_issue = _schema_issue(raw, window, index)
        if schema_issue is not None:
            rejected.append(schema_issue)
            continue

        evidence, evidence_issue, issue_kind = _validate_evidence(
            raw["evidence"],
            window,
            messages_by_id,
            index,
        )
        if evidence_issue is not None:
            if issue_kind == "rejected":
                rejected.append(evidence_issue)
            else:
                quarantined.append(evidence_issue)
            continue

        if not any(
            item.evidence_role == "primary"
            and _is_candidate_span(item, window.candidate_refs)
            for item in evidence
        ):
            rejected.append(
                _issue(
                    window,
                    index,
                    "missing_candidate_evidence",
                    "memory requires primary evidence from a candidate span",
                )
            )
            continue

        if len(raw["text"]) > config.max_memory_chars:
            quarantined.append(
                _issue(
                    window,
                    index,
                    "memory_text_too_long",
                    f"memory text exceeds {config.max_memory_chars} characters",
                )
            )
            continue

        if _suspicious_modality(raw["modality"], evidence):
            quarantined.append(
                _issue(
                    window,
                    index,
                    "suspicious_modality",
                    "memory modality is inconsistent with its evidence",
                )
            )
            continue

        canonical_content = {
            "memory_type": raw["memory_type"],
            "text": raw["text"].strip(),
            "subject": dict(raw["subject"]),
            "predicate": raw["predicate"].strip(),
            "object": raw.get("object"),
            "modality": raw["modality"],
            "event_time": dict(raw["event_time"]) if raw.get("event_time") else None,
            "attributes": dict(raw.get("attributes") or {}),
        }
        valid.append(
            AtomicMemory(
                id="candidate-" + stable_hash(window.scope_id, canonical_content),
                scope_id=window.scope_id,
                evidence=tuple(evidence),
                model_confidence=raw.get("model_confidence"),
                observed_at=_observed_at(evidence, messages_by_id, window),
                source_episode_id=window.episode_id,
                source_window_id=window.id,
                **canonical_content,
            )
        )

    return ValidationBatch(valid=valid, rejected=rejected, quarantined=quarantined)


def _schema_issue(
    raw: Any,
    window: ExtractionWindow,
    index: int,
) -> PipelineIssue | None:
    if not isinstance(raw, dict):
        return _issue(window, index, "malformed_schema", "memory must be an object")

    text = raw.get("text")
    subject = raw.get("subject")
    predicate = raw.get("predicate")
    object_value = raw.get("object")
    evidence = raw.get("evidence")
    attributes = raw.get("attributes", {})
    event_time = raw.get("event_time")
    confidence = raw.get("model_confidence")
    if (
        not isinstance(text, str)
        or not text.strip()
        or not isinstance(subject, dict)
        or not isinstance(predicate, str)
        or not predicate.strip()
        or not (
            object_value is None
            or isinstance(object_value, (str, dict))
        )
        or not isinstance(evidence, list)
        or not evidence
        or not isinstance(attributes, dict)
        or (event_time is not None and not isinstance(event_time, dict))
        or (
            confidence is not None
            and (
                not isinstance(confidence, (int, float))
                or isinstance(confidence, bool)
                or not 0.0 <= float(confidence) <= 1.0
            )
        )
    ):
        return _issue(
            window,
            index,
            "malformed_schema",
            "memory fields do not match atomic_memory_v1",
        )

    if raw.get("memory_type") not in MEMORY_TYPES or raw.get("modality") not in MODALITIES:
        return _issue(
            window,
            index,
            "unknown_enum",
            "memory_type or modality is not allowed",
        )
    return None


def _validate_evidence(
    raw_evidence: Sequence[dict[str, Any]],
    window: ExtractionWindow,
    messages_by_id: Mapping[str, NormalizedMessage],
    index: int,
) -> tuple[list[EvidenceRef], PipelineIssue | None, str]:
    window_refs = (
        *window.context_before_refs,
        *window.candidate_refs,
        *window.context_after_refs,
    )
    output: list[EvidenceRef] = []
    for raw in raw_evidence:
        if not isinstance(raw, dict):
            return [], _issue(window, index, "malformed_schema", "evidence must be an object"), "rejected"
        message_id = raw.get("message_id")
        quote = raw.get("quote")
        role = raw.get("evidence_role")
        if (
            not isinstance(message_id, str)
            or not isinstance(quote, str)
            or not quote
            or role not in EVIDENCE_ROLES
        ):
            return [], _issue(window, index, "malformed_schema", "evidence fields are invalid"), "rejected"

        refs = [ref for ref in window_refs if ref.message_id == message_id]
        if not refs or message_id not in messages_by_id:
            return [], _issue(
                window,
                index,
                "unknown_evidence_message",
                f"evidence message {message_id!r} is not in the window",
            ), "rejected"

        matches: list[tuple[MessageRef, int]] = []
        for ref in refs:
            search_at = 0
            while True:
                position = ref.text.find(quote, search_at)
                if position < 0:
                    break
                matches.append((ref, position))
                search_at = position + 1
        if not matches:
            return [], _issue(
                window,
                index,
                "evidence_quote_not_found",
                "evidence quote is not an exact substring of the referenced window span",
            ), "quarantine"
        if len(matches) != 1:
            return [], _issue(
                window,
                index,
                "ambiguous_evidence_quote",
                "evidence quote occurs more than once in the referenced window spans",
            ), "quarantine"

        ref, local_start = matches[0]
        start_char = ref.start_char + local_start
        output.append(
            EvidenceRef(
                message_id=message_id,
                quote=quote,
                start_char=start_char,
                end_char=start_char + len(quote),
                evidence_role=role,
            )
        )
    return output, None, ""


def _is_candidate_span(
    evidence: EvidenceRef,
    candidates: Sequence[MessageRef],
) -> bool:
    return any(
        ref.message_id == evidence.message_id
        and ref.start_char <= evidence.start_char
        and evidence.end_char <= ref.end_char
        for ref in candidates
    )


def _suspicious_modality(
    modality: str,
    evidence: Sequence[EvidenceRef],
) -> bool:
    if modality != "asserted":
        return False
    text = " " + " ".join(item.quote for item in evidence).lower() + " "
    return any(marker in text for marker in (*_PLAN_MARKERS, *_POSSIBLE_MARKERS, *_NEGATION_MARKERS))


def _observed_at(
    evidence: Sequence[EvidenceRef],
    messages_by_id: Mapping[str, NormalizedMessage],
    window: ExtractionWindow,
) -> str:
    for item in reversed(evidence):
        timestamp = messages_by_id[item.message_id].timestamp
        if timestamp:
            return timestamp
    for ref in reversed(window.candidate_refs):
        timestamp = messages_by_id[ref.message_id].timestamp
        if timestamp:
            return timestamp
    return ""


def _issue(
    window: ExtractionWindow,
    index: int,
    code: str,
    message: str,
) -> PipelineIssue:
    return PipelineIssue(
        stage="validation",
        code=code,
        message=message,
        scope_id=window.scope_id,
        episode_id=window.episode_id,
        window_id=window.id,
        details={"memory_index": index},
    )

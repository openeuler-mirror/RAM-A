"""Immutable contracts shared by the memory preparation pipeline."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass(frozen=True)
class PipelineIssue:
    stage: str
    code: str
    message: str
    source_id: str = ""
    scope_id: str = ""
    episode_id: str = ""
    window_id: str = ""
    details: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "stage": self.stage,
            "code": self.code,
            "message": self.message,
            "source_id": self.source_id,
            "scope_id": self.scope_id,
            "episode_id": self.episode_id,
            "window_id": self.window_id,
            "details": dict(self.details),
        }


@dataclass(frozen=True)
class NormalizedMessage:
    id: str
    scope_id: str
    text: str
    role: str
    speaker: str = ""
    timestamp: str = ""
    session_id: str = ""
    turn_index: int | None = None
    source_index: int = 0
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "scope_id": self.scope_id,
            "text": self.text,
            "role": self.role,
            "speaker": self.speaker,
            "timestamp": self.timestamp,
            "session_id": self.session_id,
            "turn_index": self.turn_index,
            "source_index": self.source_index,
            "metadata": dict(self.metadata),
        }


@dataclass(frozen=True)
class ConversationEpisode:
    id: str
    scope_id: str
    session_id: str
    message_ids: tuple[str, ...]
    start_time: str = ""
    end_time: str = ""
    boundary_reason: str = "start"
    episode_version: str = "episode_v1"

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "scope_id": self.scope_id,
            "session_id": self.session_id,
            "message_ids": list(self.message_ids),
            "start_time": self.start_time,
            "end_time": self.end_time,
            "boundary_reason": self.boundary_reason,
            "episode_version": self.episode_version,
        }


@dataclass(frozen=True)
class MessageRef:
    message_id: str
    start_char: int
    end_char: int
    text: str

    def to_dict(self) -> dict[str, Any]:
        return {
            "message_id": self.message_id,
            "start_char": self.start_char,
            "end_char": self.end_char,
            "text": self.text,
        }


@dataclass(frozen=True)
class ExtractionWindow:
    id: str
    scope_id: str
    session_id: str
    episode_id: str
    candidate_refs: tuple[MessageRef, ...]
    context_before_refs: tuple[MessageRef, ...] = ()
    context_after_refs: tuple[MessageRef, ...] = ()
    candidate_token_count: int = 0
    total_token_count: int = 0
    window_version: str = "window_v1"

    @property
    def candidate_message_ids(self) -> tuple[str, ...]:
        return tuple(dict.fromkeys(ref.message_id for ref in self.candidate_refs))

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "scope_id": self.scope_id,
            "session_id": self.session_id,
            "episode_id": self.episode_id,
            "candidate_refs": [ref.to_dict() for ref in self.candidate_refs],
            "context_before_refs": [ref.to_dict() for ref in self.context_before_refs],
            "context_after_refs": [ref.to_dict() for ref in self.context_after_refs],
            "candidate_message_ids": list(self.candidate_message_ids),
            "candidate_token_count": self.candidate_token_count,
            "total_token_count": self.total_token_count,
            "window_version": self.window_version,
        }


@dataclass(frozen=True)
class EvidenceRef:
    message_id: str
    quote: str
    start_char: int
    end_char: int
    evidence_role: str

    def to_dict(self) -> dict[str, Any]:
        return {
            "message_id": self.message_id,
            "quote": self.quote,
            "start_char": self.start_char,
            "end_char": self.end_char,
            "evidence_role": self.evidence_role,
        }


@dataclass(frozen=True)
class AtomicMemory:
    id: str
    scope_id: str
    text: str
    memory_type: str
    subject: dict[str, Any]
    predicate: str
    object: dict[str, Any] | str | None
    modality: str
    evidence: tuple[EvidenceRef, ...]
    event_time: dict[str, Any] | None = None
    attributes: dict[str, Any] = field(default_factory=dict)
    model_confidence: float | None = None
    observed_at: str = ""
    source_episode_id: str = ""
    source_window_id: str = ""
    observation_refs: tuple[dict[str, Any], ...] = ()

    def canonical_content(self) -> dict[str, Any]:
        return {
            "memory_type": self.memory_type,
            "text": self.text,
            "subject": self.subject,
            "predicate": self.predicate,
            "object": self.object,
            "modality": self.modality,
            "event_time": self.event_time,
            "attributes": self.attributes,
        }

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "scope_id": self.scope_id,
            **self.canonical_content(),
            "evidence": [item.to_dict() for item in self.evidence],
            "model_confidence": self.model_confidence,
            "observed_at": self.observed_at,
            "source_episode_id": self.source_episode_id,
            "source_window_id": self.source_window_id,
            "observation_refs": [dict(item) for item in self.observation_refs],
        }

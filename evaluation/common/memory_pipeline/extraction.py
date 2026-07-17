"""Structured atomic-memory extraction contracts and implementations."""

from __future__ import annotations

from dataclasses import dataclass, field
import json
import re
from typing import Any, Mapping, Protocol

from .models import ExtractionWindow, NormalizedMessage
from .window import render_window


SCHEMA_VERSION = "atomic_memory_v1"
_FENCE_RE = re.compile(r"^```(?:json)?\s*\n(?P<body>.*)\n```\s*$", re.DOTALL)


class ExtractionProtocolError(ValueError):
    """Raised when an extractor response violates the transport contract."""


@dataclass(frozen=True)
class ModelUsage:
    latency_ms: float = 0.0
    prompt_tokens: int = 0
    completion_tokens: int = 0
    total_tokens: int = 0

    def to_dict(self) -> dict[str, Any]:
        return {
            "latency_ms": self.latency_ms,
            "prompt_tokens": self.prompt_tokens,
            "completion_tokens": self.completion_tokens,
            "total_tokens": self.total_tokens,
        }


@dataclass(frozen=True)
class ExtractionBatch:
    window_id: str
    schema_version: str
    raw_memories: list[dict[str, Any]]
    usage: ModelUsage = field(default_factory=ModelUsage)
    raw_response: str = ""

    def to_dict(self) -> dict[str, Any]:
        return {
            "window_id": self.window_id,
            "schema_version": self.schema_version,
            "raw_memories": self.raw_memories,
            "usage": self.usage.to_dict(),
            "raw_response": self.raw_response,
        }


class MemoryExtractor(Protocol):
    model: str
    prompt_version: str

    def extract(
        self,
        window: ExtractionWindow,
        messages_by_id: Mapping[str, NormalizedMessage],
    ) -> ExtractionBatch:
        raise NotImplementedError


class ChatClient(Protocol):
    def chat(
        self,
        model: str,
        messages: list[dict],
        temperature: float = 0.0,
        max_tokens: int = 512,
    ) -> Any:
        raise NotImplementedError


class StaticMemoryExtractor:
    model = "static"
    prompt_version = "static_v1"

    def __init__(self, responses: Mapping[str, dict[str, Any]]) -> None:
        self._responses = dict(responses)

    def extract(
        self,
        window: ExtractionWindow,
        messages_by_id: Mapping[str, NormalizedMessage],
    ) -> ExtractionBatch:
        del messages_by_id
        try:
            payload = self._responses[window.id]
        except KeyError as error:
            raise ExtractionProtocolError(
                f"missing static extraction for window {window.id}"
            ) from error
        return _batch_from_payload(window.id, payload)


class LLMMemoryExtractor:
    def __init__(
        self,
        client: ChatClient,
        model: str,
        prompt_version: str = "extract_v2",
        max_output_tokens: int = 1600,
    ) -> None:
        self.client = client
        self.model = model
        self.prompt_version = prompt_version
        self.max_output_tokens = max_output_tokens

    def extract(
        self,
        window: ExtractionWindow,
        messages_by_id: Mapping[str, NormalizedMessage],
    ) -> ExtractionBatch:
        result = self.client.chat(
            model=self.model,
            messages=[
                {
                    "role": "system",
                    "content": _SYSTEM_PROMPT,
                },
                {
                    "role": "user",
                    "content": build_extraction_prompt(
                        window,
                        messages_by_id,
                        observed_at=_observed_at(window, messages_by_id),
                    ),
                },
            ],
            temperature=0.0,
            max_tokens=self.max_output_tokens,
        )
        payload = parse_extraction_json(result.content)
        batch = _batch_from_payload(window.id, payload, raw_response=result.content)
        return ExtractionBatch(
            window_id=batch.window_id,
            schema_version=batch.schema_version,
            raw_memories=batch.raw_memories,
            usage=ModelUsage(
                latency_ms=float(result.latency_ms),
                prompt_tokens=int(result.prompt_tokens),
                completion_tokens=int(result.completion_tokens),
                total_tokens=int(result.total_tokens),
            ),
            raw_response=batch.raw_response,
        )


def parse_extraction_json(content: str) -> dict[str, Any]:
    text = content.strip()
    fence = _FENCE_RE.fullmatch(text)
    if fence:
        text = fence.group("body").strip()
    try:
        value = json.loads(text)
    except json.JSONDecodeError as error:
        raise ExtractionProtocolError(f"extractor did not return valid JSON: {error}") from error
    if not isinstance(value, dict):
        raise ExtractionProtocolError("extractor response must be a JSON object")
    return value


def build_extraction_prompt(
    window: ExtractionWindow,
    messages_by_id: Mapping[str, NormalizedMessage],
    observed_at: str,
) -> str:
    schema_template = json.dumps(
        {
            "schema_version": SCHEMA_VERSION,
            "memories": [
                {
                    "text": "...",
                    "memory_type": "fact",
                    "subject": {"name": "...", "source_speaker": "..."},
                    "predicate": "...",
                    "object": {"name": "...", "type": "..."},
                    "modality": "asserted",
                    "event_time": {
                        "raw": "...",
                        "normalized": "...",
                        "precision": "...",
                    },
                    "attributes": {},
                    "evidence": [
                        {
                            "message_id": "copy an exact message_id from the window",
                            "quote": "copy an exact substring from that message span",
                            "evidence_role": "primary",
                        }
                    ],
                    "model_confidence": 0.95,
                }
            ],
        },
        ensure_ascii=False,
    )
    return f"""Extract durable atomic memories from the candidate messages.

Rules:
- Only candidate messages may create new memories. Context is for resolving references only.
- Each memory must express one self-contained fact, preference, relationship, event, state, or procedure.
- Preserve negation, plans, possibilities, conditions, names, numbers, and dates.
- Do not add facts from world knowledge or from context alone.
- Each memory needs at least one primary evidence item from a candidate message.
- Evidence quote must be an exact quote from the referenced source message.
- Return {json.dumps({"schema_version": SCHEMA_VERSION, "memories": []})} when nothing is durable.
- Return one JSON object and no commentary.
- Replace every "..." placeholder in the template below with source-grounded data.
- subject MUST be an object, never a string.
- object MUST be an object, string, or null.
- event_time MUST be an object or null, never a string.
- evidence MUST be a non-empty array of objects. message_id must be copied exactly
  from a window header; quote must be an exact substring of that message span;
  evidence_role must be primary or supporting. At least one primary item must cite
  a candidate message, not context-only text.
- model_confidence MUST be a number from 0.0 to 1.0, never words such as "high".
- memory_type MUST be one of: fact, preference, relationship, event, state,
  procedure, other. It cannot be "planned"; planned belongs in modality.
- modality MUST be one of: asserted, negated, possible, planned, conditional, reported.

Required JSON shape:
{schema_template}

Host observation time: {observed_at or "unknown"}

{render_window(window, messages_by_id)}

"""


def _batch_from_payload(
    window_id: str,
    payload: dict[str, Any],
    raw_response: str = "",
) -> ExtractionBatch:
    schema_version = payload.get("schema_version")
    if schema_version != SCHEMA_VERSION:
        raise ExtractionProtocolError(
            f"unexpected extraction schema_version: {schema_version!r}"
        )
    memories = payload.get("memories")
    if not isinstance(memories, list):
        raise ExtractionProtocolError("extraction memories must be a list")
    if any(not isinstance(item, dict) for item in memories):
        raise ExtractionProtocolError("each extracted memory must be an object")
    return ExtractionBatch(
        window_id=window_id,
        schema_version=schema_version,
        raw_memories=[dict(item) for item in memories],
        raw_response=raw_response,
    )


def _observed_at(
    window: ExtractionWindow,
    messages_by_id: Mapping[str, NormalizedMessage],
) -> str:
    for ref in reversed(window.candidate_refs):
        timestamp = messages_by_id[ref.message_id].timestamp
        if timestamp:
            return timestamp
    return ""


_SYSTEM_PROMPT = (
    "You are a source-faithful long-term-memory extractor. "
    "Output only the requested JSON object. Never invent evidence identifiers."
)

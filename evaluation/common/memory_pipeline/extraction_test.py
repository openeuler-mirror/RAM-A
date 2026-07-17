from __future__ import annotations

import json
from dataclasses import dataclass

import pytest

from common.memory_pipeline.extraction import (
    ExtractionProtocolError,
    LLMMemoryExtractor,
    StaticMemoryExtractor,
    build_extraction_prompt,
    parse_extraction_json,
)
from common.memory_pipeline.models import ExtractionWindow, MessageRef, NormalizedMessage


MESSAGE_LOOKUP = {
    "context": NormalizedMessage(
        id="context",
        scope_id="u1",
        session_id="s1",
        text="Alice moved last year.",
        role="user",
        speaker="Alice",
    ),
    "candidate": NormalizedMessage(
        id="candidate",
        scope_id="u1",
        session_id="s1",
        text="I plan to move to Hangzhou.",
        role="user",
        speaker="Alice",
    ),
}
WINDOW = ExtractionWindow(
    id="window-1",
    scope_id="u1",
    session_id="s1",
    episode_id="episode-1",
    context_before_refs=(MessageRef("context", 0, 22, "Alice moved last year."),),
    candidate_refs=(MessageRef("candidate", 0, 27, "I plan to move to Hangzhou."),),
)


def test_parse_extraction_accepts_plain_and_fenced_json() -> None:
    expected = {"schema_version": "atomic_memory_v1", "memories": []}

    assert parse_extraction_json(json.dumps(expected)) == expected
    assert parse_extraction_json(f"```json\n{json.dumps(expected)}\n```") == expected


def test_parse_extraction_rejects_non_object_and_trailing_text() -> None:
    with pytest.raises(ExtractionProtocolError, match="JSON object"):
        parse_extraction_json("[]")
    with pytest.raises(ExtractionProtocolError, match="valid JSON"):
        parse_extraction_json('{"memories": []} trailing')


def test_prompt_separates_context_and_candidate() -> None:
    prompt = build_extraction_prompt(
        WINDOW,
        MESSAGE_LOOKUP,
        observed_at="2026-07-14T10:00:00Z",
    )

    assert "<context>" in prompt
    assert "<candidate>" in prompt
    assert "message_id=context" in prompt
    assert "message_id=candidate" in prompt
    assert "Only candidate messages may create new memories" in prompt
    assert "exact quote" in prompt
    assert "2026-07-14T10:00:00Z" in prompt
    assert '"subject": {"name": "...", "source_speaker": "..."}' in prompt
    assert '"evidence": [' in prompt
    assert '"message_id": "copy an exact message_id from the window"' in prompt
    assert '"evidence_role": "primary"' in prompt
    assert "subject MUST be an object" in prompt
    assert "model_confidence MUST be a number from 0.0 to 1.0" in prompt


def test_static_extractor_returns_fixture_without_network() -> None:
    payload = {"schema_version": "atomic_memory_v1", "memories": []}
    extractor = StaticMemoryExtractor({WINDOW.id: payload})

    result = extractor.extract(WINDOW, MESSAGE_LOOKUP)

    assert result.raw_memories == []
    assert result.schema_version == "atomic_memory_v1"
    assert result.usage.total_tokens == 0


@dataclass
class _ChatResult:
    content: str
    latency_ms: float = 12.5
    prompt_tokens: int = 100
    completion_tokens: int = 20
    total_tokens: int = 120
    raw: dict | None = None


class _FakeChatClient:
    def __init__(self, content: str) -> None:
        self.content = content
        self.calls: list[dict] = []

    def chat(self, **kwargs):
        self.calls.append(kwargs)
        return _ChatResult(self.content)


def test_llm_extractor_calls_chat_deterministically_and_records_usage() -> None:
    client = _FakeChatClient(
        json.dumps({"schema_version": "atomic_memory_v1", "memories": []})
    )
    extractor = LLMMemoryExtractor(
        client=client,
        model="test/model",
        prompt_version="extract_v1",
        max_output_tokens=700,
    )

    result = extractor.extract(WINDOW, MESSAGE_LOOKUP)

    assert result.raw_memories == []
    assert result.usage.total_tokens == 120
    assert client.calls[0]["model"] == "test/model"
    assert client.calls[0]["temperature"] == 0.0
    assert client.calls[0]["max_tokens"] == 700
    assert client.calls[0]["messages"][0]["role"] == "system"


def test_static_extractor_requires_response_for_window() -> None:
    extractor = StaticMemoryExtractor({})

    with pytest.raises(ExtractionProtocolError, match="missing static extraction"):
        extractor.extract(WINDOW, MESSAGE_LOOKUP)

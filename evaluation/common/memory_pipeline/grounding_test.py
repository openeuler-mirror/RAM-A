from __future__ import annotations

import json
from dataclasses import dataclass

import pytest

from common.memory_pipeline.grounding import (
    GroundingProtocolError,
    LLMGroundingVerifier,
    StaticGroundingVerifier,
)
from common.memory_pipeline.models import (
    AtomicMemory,
    EvidenceRef,
    ExtractionWindow,
    MessageRef,
    NormalizedMessage,
)


LOOKUP = {
    "m1": NormalizedMessage(
        id="m1",
        scope_id="u1",
        text="I moved to Hangzhou.",
        role="user",
        speaker="Alice",
        timestamp="2026-07-01T10:00:00Z",
    ),
    "unrelated": NormalizedMessage(
        id="unrelated",
        scope_id="u1",
        text="SECRET UNRELATED CONTEXT",
        role="user",
    ),
}
WINDOW = ExtractionWindow(
    id="window-1",
    scope_id="u1",
    session_id="s1",
    episode_id="episode-1",
    candidate_refs=(MessageRef("m1", 0, 20, "I moved to Hangzhou."),),
    context_before_refs=(
        MessageRef("unrelated", 0, 24, "SECRET UNRELATED CONTEXT"),
    ),
)


def _memory(memory_id: str, text: str) -> AtomicMemory:
    return AtomicMemory(
        id=memory_id,
        scope_id="u1",
        text=text,
        memory_type="event",
        subject={"name": "user"},
        predicate="moved_to",
        object={"name": "Hangzhou"},
        modality="asserted",
        evidence=(
            EvidenceRef(
                message_id="m1",
                quote="moved to Hangzhou",
                start_char=2,
                end_char=19,
                evidence_role="primary",
            ),
        ),
    )


MEMORY_1 = _memory("mem-1", "The user moved to Hangzhou.")
MEMORY_2 = _memory("mem-2", "The user likes Hangzhou.")


def test_static_verifier_maps_each_memory_id() -> None:
    verifier = StaticGroundingVerifier(
        {"mem-1": "SUPPORTED", "mem-2": "UNSUPPORTED"}
    )

    batch = verifier.verify(WINDOW, [MEMORY_1, MEMORY_2], LOOKUP)

    assert [result.status for result in batch.results] == [
        "SUPPORTED",
        "UNSUPPORTED",
    ]
    assert batch.usage.total_tokens == 0


@dataclass
class _ChatResult:
    content: str
    latency_ms: float = 5.0
    prompt_tokens: int = 20
    completion_tokens: int = 10
    total_tokens: int = 30
    raw: dict | None = None


class _FakeChatClient:
    def __init__(self, content: str) -> None:
        self.content = content
        self.calls: list[dict] = []

    def chat(self, **kwargs):
        self.calls.append(kwargs)
        return _ChatResult(self.content)


def test_llm_verifier_defaults_missing_result_to_uncertain() -> None:
    verifier = LLMGroundingVerifier(
        _FakeChatClient('{"results": []}'),
        model="test/model",
    )

    batch = verifier.verify(WINDOW, [MEMORY_1], LOOKUP)

    assert len(batch.results) == 1
    assert batch.results[0].memory_id == "mem-1"
    assert batch.results[0].status == "UNCERTAIN"
    assert "omitted" in batch.results[0].reason.lower()


def test_llm_verifier_partial_response_keeps_returned_and_fills_missing() -> None:
    response = {
        "results": [
            {
                "memory_id": "mem-1",
                "status": "SUPPORTED",
                "reason": "The evidence directly states the move.",
            }
        ]
    }
    verifier = LLMGroundingVerifier(
        _FakeChatClient(json.dumps(response)),
        model="test/model",
    )

    batch = verifier.verify(WINDOW, [MEMORY_1, MEMORY_2], LOOKUP)

    assert [result.memory_id for result in batch.results] == ["mem-1", "mem-2"]
    assert batch.results[0].status == "SUPPORTED"
    assert batch.results[1].status == "UNCERTAIN"
    assert "omitted" in batch.results[1].reason.lower()


def test_llm_verifier_rejects_unknown_status() -> None:
    response = {"results": [{"memory_id": "mem-1", "status": "MAYBE"}]}
    verifier = LLMGroundingVerifier(
        _FakeChatClient(json.dumps(response)),
        model="test/model",
    )

    with pytest.raises(GroundingProtocolError, match="unknown grounding status"):
        verifier.verify(WINDOW, [MEMORY_1], LOOKUP)


def test_llm_verifier_records_usage_and_sends_only_selected_evidence() -> None:
    response = {
        "results": [
            {
                "memory_id": "mem-1",
                "status": "SUPPORTED",
                "reason": "The evidence directly states the move.",
            }
        ]
    }
    client = _FakeChatClient(json.dumps(response))
    verifier = LLMGroundingVerifier(client, model="test/model", prompt_version="ground_v1")

    batch = verifier.verify(WINDOW, [MEMORY_1], LOOKUP)

    assert batch.results[0].status == "SUPPORTED"
    assert batch.usage.total_tokens == 30
    prompt = client.calls[0]["messages"][1]["content"]
    assert "moved to Hangzhou" in prompt
    assert "Alice" in prompt
    assert "2026-07-01T10:00:00Z" in prompt
    assert "SECRET UNRELATED CONTEXT" not in prompt
    assert client.calls[0]["temperature"] == 0.0


def test_static_verifier_rejects_missing_fixture() -> None:
    verifier = StaticGroundingVerifier({})

    with pytest.raises(GroundingProtocolError, match="missing static grounding"):
        verifier.verify(WINDOW, [MEMORY_1], LOOKUP)

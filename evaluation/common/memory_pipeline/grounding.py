"""Evidence-scoped grounding verification for candidate atomic memories."""

from __future__ import annotations

from dataclasses import dataclass, field
import json
from typing import Any, Mapping, Protocol, Sequence

from .extraction import ChatClient, ModelUsage, parse_extraction_json
from .models import AtomicMemory, ExtractionWindow, NormalizedMessage


GROUNDING_STATUSES = {
    "SUPPORTED",
    "PARTIALLY_SUPPORTED",
    "UNSUPPORTED",
    "UNCERTAIN",
}


class GroundingProtocolError(ValueError):
    """Raised when a verifier response cannot be mapped to candidate memories."""


@dataclass(frozen=True)
class GroundingResult:
    memory_id: str
    status: str
    reason: str = ""

    def to_dict(self) -> dict[str, str]:
        return {
            "memory_id": self.memory_id,
            "status": self.status,
            "reason": self.reason,
        }


@dataclass(frozen=True)
class GroundingBatch:
    window_id: str
    results: list[GroundingResult]
    usage: ModelUsage = field(default_factory=ModelUsage)
    raw_response: str = ""

    def to_dict(self) -> dict[str, Any]:
        return {
            "window_id": self.window_id,
            "results": [item.to_dict() for item in self.results],
            "usage": self.usage.to_dict(),
            "raw_response": self.raw_response,
        }


class GroundingVerifier(Protocol):
    model: str
    prompt_version: str

    def verify(
        self,
        window: ExtractionWindow,
        memories: Sequence[AtomicMemory],
        messages_by_id: Mapping[str, NormalizedMessage],
    ) -> GroundingBatch:
        raise NotImplementedError


class StaticGroundingVerifier:
    model = "static"
    prompt_version = "static_v1"

    def __init__(self, responses: Mapping[str, str | Mapping[str, str]]) -> None:
        self._responses = dict(responses)

    def verify(
        self,
        window: ExtractionWindow,
        memories: Sequence[AtomicMemory],
        messages_by_id: Mapping[str, NormalizedMessage],
    ) -> GroundingBatch:
        del messages_by_id
        results: list[GroundingResult] = []
        for memory in memories:
            try:
                response = self._responses[memory.id]
            except KeyError as error:
                raise GroundingProtocolError(
                    f"missing static grounding for memory {memory.id}"
                ) from error
            if isinstance(response, str):
                status = response
                reason = ""
            else:
                status = str(response.get("status") or "")
                reason = str(response.get("reason") or "")
            _validate_status(status)
            results.append(GroundingResult(memory.id, status, reason))
        return GroundingBatch(window_id=window.id, results=results)


class LLMGroundingVerifier:
    def __init__(
        self,
        client: ChatClient,
        model: str,
        prompt_version: str = "ground_v1",
        max_output_tokens: int = 1000,
    ) -> None:
        self.client = client
        self.model = model
        self.prompt_version = prompt_version
        self.max_output_tokens = max_output_tokens

    def verify(
        self,
        window: ExtractionWindow,
        memories: Sequence[AtomicMemory],
        messages_by_id: Mapping[str, NormalizedMessage],
    ) -> GroundingBatch:
        if not memories:
            return GroundingBatch(window_id=window.id, results=[])
        result = self.client.chat(
            model=self.model,
            messages=[
                {
                    "role": "system",
                    "content": (
                        "You verify whether each candidate memory is fully supported by "
                        "its quoted source evidence. Return only JSON."
                    ),
                },
                {
                    "role": "user",
                    "content": _build_grounding_prompt(memories, messages_by_id),
                },
            ],
            temperature=0.0,
            max_tokens=self.max_output_tokens,
        )
        try:
            payload = parse_extraction_json(result.content)
        except ValueError as error:
            raise GroundingProtocolError(f"invalid grounding JSON: {error}") from error
        parsed_results = _parse_results(payload, memories)
        return GroundingBatch(
            window_id=window.id,
            results=parsed_results,
            usage=ModelUsage(
                latency_ms=float(result.latency_ms),
                prompt_tokens=int(result.prompt_tokens),
                completion_tokens=int(result.completion_tokens),
                total_tokens=int(result.total_tokens),
            ),
            raw_response=result.content,
        )


def _build_grounding_prompt(
    memories: Sequence[AtomicMemory],
    messages_by_id: Mapping[str, NormalizedMessage],
) -> str:
    candidates = []
    for memory in memories:
        candidates.append(
            {
                "memory_id": memory.id,
                "claim": memory.canonical_content(),
                "observation_time": memory.observed_at,
                "evidence": [
                    {
                        "message_id": evidence.message_id,
                        "quote": evidence.quote,
                        "start_char": evidence.start_char,
                        "end_char": evidence.end_char,
                        "evidence_role": evidence.evidence_role,
                        "source_role": messages_by_id[evidence.message_id].role,
                        "source_speaker": messages_by_id[evidence.message_id].speaker,
                        "source_timestamp": messages_by_id[evidence.message_id].timestamp,
                    }
                    for evidence in memory.evidence
                ],
            }
        )
    return (
        "Classify every memory as SUPPORTED, PARTIALLY_SUPPORTED, UNSUPPORTED, "
        "or UNCERTAIN. SUPPORTED means every material part of the claim follows "
        "from the quoted evidence. Return one result per memory_id.\n\n"
        + json.dumps({"memories": candidates}, ensure_ascii=False, sort_keys=True)
        + '\n\nOutput: {"results":[{"memory_id":"...","status":"SUPPORTED","reason":"..."}]}'
    )


def _parse_results(
    payload: dict[str, Any],
    memories: Sequence[AtomicMemory],
) -> list[GroundingResult]:
    raw_results = payload.get("results")
    if not isinstance(raw_results, list):
        raise GroundingProtocolError("grounding results must be a list")

    expected_ids = [memory.id for memory in memories]
    by_id: dict[str, GroundingResult] = {}
    for raw in raw_results:
        if not isinstance(raw, dict):
            raise GroundingProtocolError("each grounding result must be an object")
        memory_id = raw.get("memory_id")
        status = raw.get("status")
        if not isinstance(memory_id, str) or not isinstance(status, str):
            raise GroundingProtocolError("grounding result fields are invalid")
        if memory_id in by_id:
            raise GroundingProtocolError(f"duplicate grounding result for {memory_id}")
        if memory_id not in expected_ids:
            raise GroundingProtocolError(f"unexpected grounding result for {memory_id}")
        _validate_status(status)
        by_id[memory_id] = GroundingResult(
            memory_id=memory_id,
            status=status,
            reason=str(raw.get("reason") or ""),
        )

    for memory_id in expected_ids:
        if memory_id not in by_id:
            by_id[memory_id] = GroundingResult(
                memory_id=memory_id,
                status="UNCERTAIN",
                reason="verifier omitted this memory_id",
            )
    return [by_id[memory_id] for memory_id in expected_ids]


def _validate_status(status: str) -> None:
    if status not in GROUNDING_STATUSES:
        raise GroundingProtocolError(f"unknown grounding status: {status!r}")

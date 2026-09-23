"""Correlation ids shared by model-call middleware and event clients."""

from __future__ import annotations

from dataclasses import dataclass
from uuid import uuid4


@dataclass(frozen=True)
class Correlation:
    session_id: str
    agent_turn_id: str
    model_request_id: str | None
    model: str | None = None
    vllm_request_id: str | None = None


def normalize_session_id(value: object | None) -> str | None:
    """Normalize a thread identity without inventing a new id for each call."""
    if value is None or str(value).strip() == "":
        return None
    raw = str(value).strip()
    return raw if raw.startswith("deepagents:") else f"deepagents:{raw}"

class IdentityContext:
    """Creates fresh correlation values without retaining cross-invocation state."""
    def begin_turn(self, session_id: str) -> str:
        turn_id = f"turn-{uuid4()}"
        return turn_id

    def begin_model_call(self, session_id: str, agent_turn_id: str, model: str | None = None, request_id: str | None = None) -> Correlation:
        correlation = Correlation(normalize_session_id(session_id), agent_turn_id, request_id, model)
        return correlation

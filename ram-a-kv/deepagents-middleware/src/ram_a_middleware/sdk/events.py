"""Protocol event and model-call result types shared by integrations."""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Literal
from uuid import UUID, uuid4

EventStatus = Literal["success", "degraded", "disabled", "error"]


@dataclass(frozen=True)
class CallContext:
    session_id: str
    agent_turn_id: str
    model_request_id: str | None = None
    model: str | None = None
    vllm_request_id: str | None = None
    context: dict[str, Any] | None = None

@dataclass(frozen=True)
class CallResult:
    status: EventStatus = "success"
    total_time_ms: int | None = None
    ttft_ms: int | None = None
    error_code: str | None = None
    message_count: int | None = None
    response: dict[str, Any] | None = None
    cache_hashes: tuple[str, ...] | None = None
    response_metadata: dict[str, Any] | None = None
    debug_max_bytes: int = 4096

@dataclass(frozen=True)
class EventResult:
    ok: bool
    status: EventStatus
    event_type: str
    event_id: str | None = None
    data: dict[str, Any] | None = None
    error_code: str | None = None
    error: str | None = None


@dataclass(frozen=True)
class AgentEvent:
    """A framework-neutral observation sent to ``agent_event``.

    The envelope deliberately keeps observation data separate from RAM-A's KV
    lifecycle events. ``event_id`` is stable across transport retries and is
    used by the daemon for idempotent acknowledgement.
    """

    event_name: str
    event_action: str
    session_id: str
    agent_turn_id: str | None = None
    model_request_id: str | None = None
    run_id: str | None = None
    parent_run_id: str | None = None
    phase: str | None = None
    source: str = "deepagents"
    sequence: int | None = None
    operation_id: str | None = None
    event_id: UUID = field(default_factory=uuid4)
    payload: dict[str, Any] = field(default_factory=dict)
    detail_level: str = "summary"
    schema_version: int = 1

"""Three-event HTTP clients with explicit connection ownership."""
from __future__ import annotations

from typing import Any
from uuid import uuid4

import httpx

from .events import AgentEvent, CallContext, CallResult, EventResult
from .serialization import json_safe
from . import traffic_log


def _valid_hashes(value: Any) -> bool:
    try:
        return isinstance(value, (list, tuple)) and 0 < len(value) <= 1000 and all(
            isinstance(item, str) and item.strip() and len(item.encode("utf-8")) <= 100
            for item in value
        )
    except UnicodeError:
        return False


def _base_fields(ctx: CallContext) -> dict:
    return {key: value for key, value in {
        "session_id": ctx.session_id, "agent_turn_id": ctx.agent_turn_id,
        "model_request_id": ctx.model_request_id, "model": ctx.model,
        "vllm_request_id": ctx.vllm_request_id,
    }.items() if value is not None}


class _BorrowedTransport(httpx.BaseTransport):
    def __init__(self, transport):
        self.transport = transport

    def handle_request(self, request):
        return self.transport.handle_request(request)

    def close(self):
        pass  # The caller owns the supplied transport.


class _BorrowedAsyncTransport(httpx.AsyncBaseTransport):
    def __init__(self, transport):
        self.transport = transport

    async def handle_async_request(self, request):
        return await self.transport.handle_async_request(request)

    async def aclose(self):
        pass


class _Protocol:
    def __init__(self, daemon_url: str, *, timeout: float = 0.35,
                 auth_token: str | None = None, retries: int = 0, transport: httpx.BaseTransport | httpx.AsyncBaseTransport | None = None):
        self.daemon_url = daemon_url
        self.timeout = timeout
        self.auth_token = auth_token
        self.retries = max(0, retries)
        self.transport = transport
        self._http = None
        self._closed = False

    def _url(self):
        return f"{self.daemon_url.rstrip('/')}/event"

    def _headers(self):
        return {"Authorization": f"Bearer {self.auth_token}"} if self.auth_token else {}

    @staticmethod
    def _invalid(kind, error):
        return EventResult(False, "error", kind, error_code=f"invalid_{kind}", error=error)

    @staticmethod
    def _failure(kind, body, exc):
        return EventResult(False, "degraded", kind, body["event_id"],
                          error_code=type(exc).__name__, error=str(exc))

    @staticmethod
    def _response(kind, body, response):
        try:
            payload = response.json()
        except ValueError:
            payload = None
        if not isinstance(payload, dict) or type(payload.get("ok")) is not bool:
            return EventResult(False, "degraded", kind, body["event_id"],
                               error_code="invalid_response", error="expected a boolean ok in daemon response")
        data = payload.get("data")
        if payload["ok"] and response.is_success:
            degraded = isinstance(data, dict) and data.get("backend_degraded") is True
            return EventResult(True, "degraded" if degraded else "success", kind,
                               body["event_id"], data if isinstance(data, dict) else None)
        return EventResult(False, "degraded" if response.status_code >= 500 else "error",
                           kind, body["event_id"],
                           error_code="daemon_error" if response.is_success else f"http_{response.status_code}",
                           error=payload.get("error"))

    def _fields(self, kind, value, result=None):
        """Validation returns an EventResult instead of leaking input errors."""
        try:
            if kind == "agent_event":
                if not isinstance(value, AgentEvent):
                    raise ValueError("event must be an AgentEvent")
                for field in (value.session_id, value.event_name, value.event_action):
                    if not isinstance(field, str) or not field.strip():
                        raise ValueError("session_id, event_name and event_action must be non-empty strings")
                if not isinstance(value.payload, dict):
                    raise ValueError("payload must be an object")
                fields = {k: v for k, v in vars(value).items() if v is not None}
                fields["event_id"] = str(value.event_id)
                if not fields["event_id"].strip() or len(fields["event_id"].encode()) > 128:
                    raise ValueError("event_id must contain 1 to 128 UTF-8 bytes")
                return fields
            if not isinstance(value, CallContext) or not isinstance(value.session_id, str) or not value.session_id.strip():
                raise ValueError("a non-empty session_id is required")
            fields = _base_fields(value)
            if kind == "turn_start":
                # Context snapshots are debug-only and sent once, with turn_end.
                return fields
            if not isinstance(result, CallResult):
                raise ValueError("a CallResult is required for turn_end")
            if result.cache_hashes is None:
                # The model did not return kv_transfer_params: still send the
                # turn_end with chunk_hashes null (middleware-side contract;
                # the daemon applies its own validation to what it receives).
                fields["kv_transfer_params"] = {"chunk_hashes": None}
            else:
                if not _valid_hashes(result.cache_hashes):
                    raise ValueError("requires 1 to 1000 cache identifiers, each at most 100 UTF-8 bytes")
                if result.status != "success":
                    raise ValueError("cannot update cache mapping for an unsuccessful model result")
                fields["kv_transfer_params"] = {"chunk_hashes": list(result.cache_hashes)}
            debug = {"timing": {"total_time_ms": result.total_time_ms}}
            for key, item in (("context", value.context), ("response", result.response),
                              ("response_metadata", result.response_metadata)):
                if item is not None:
                    debug[key] = item
            fields["debug_context"] = json_safe(debug, max_bytes=result.debug_max_bytes)
            return fields
        except Exception as exc:
            return self._invalid(kind, str(exc))


class RamAProtocolClient(_Protocol):
    """Reusable sync connection pool. Close it or use with block.

    A supplied transport is borrowed and must be closed by its owner.
    """

    def close(self):
        self._closed = True
        if self._http is not None:
            self._http.close()

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        self.close()

    def _post(self, kind, fields, *, retryable=False):
        if isinstance(fields, EventResult):
            traffic_log.request(self._url(), {"type": kind, "validation_error": fields.error})
            return fields
        body = {**fields, "type": kind,
                "event_id": fields["event_id"] if "event_id" in fields else str(uuid4())}
        traffic_log.request(self._url(), body)
        try:
            if self._closed:
                raise RuntimeError("RAM-A client is closed")
            if self._http is None:
                transport = _BorrowedTransport(self.transport) if self.transport is not None else None
                self._http = httpx.Client(timeout=self.timeout, transport=transport)
            attempts = self.retries if retryable else 0
            for attempt in range(attempts + 1):
                try:
                    response = self._http.post(self._url(), json=body, headers=self._headers())
                    result = self._response(kind, body, response)
                except httpx.HTTPError as exc:
                    result = self._failure(kind, body, exc)
                traffic_log.response(kind, body["event_id"], result)
                if result.ok or result.status != "degraded" or attempt == attempts:
                    return result
        except Exception as exc:
            return self._failure(kind, body, exc)

    def begin_model_call(self, ctx: CallContext) -> EventResult:
        return self._post("turn_start", self._fields("turn_start", ctx))

    def end_model_call(self, ctx: CallContext, result: CallResult) -> EventResult:
        return self._post("turn_end", self._fields("turn_end", ctx, result))

    def agent_event(self, event: AgentEvent) -> EventResult:
        return self._post("agent_event", self._fields("agent_event", event), retryable=True)


class AsyncRamAProtocolClient(_Protocol):
    """Reusable async pool; use within one event loop and close with aclose()."""

    async def aclose(self):
        self._closed = True
        if self._http is not None:
            await self._http.aclose()

    async def __aenter__(self):
        return self

    async def __aexit__(self, *_args):
        await self.aclose()

    async def _post(self, kind, fields, *, retryable=False):
        if isinstance(fields, EventResult):
            traffic_log.request(self._url(), {"type": kind, "validation_error": fields.error})
            return fields
        body = {**fields, "type": kind,
                "event_id": fields["event_id"] if "event_id" in fields else str(uuid4())}
        traffic_log.request(self._url(), body)
        try:
            if self._closed:
                raise RuntimeError("RAM-A client is closed")
            if self._http is None:
                transport = _BorrowedAsyncTransport(self.transport) if self.transport is not None else None
                self._http = httpx.AsyncClient(timeout=self.timeout, transport=transport)
            attempts = self.retries if retryable else 0
            for attempt in range(attempts + 1):
                try:
                    response = await self._http.post(self._url(), json=body, headers=self._headers())
                    result = self._response(kind, body, response)
                except httpx.HTTPError as exc:
                    result = self._failure(kind, body, exc)
                traffic_log.response(kind, body["event_id"], result)
                if result.ok or result.status != "degraded" or attempt == attempts:
                    return result
        except Exception as exc:
            return self._failure(kind, body, exc)

    async def begin_model_call(self, ctx: CallContext) -> EventResult:
        return await self._post("turn_start", self._fields("turn_start", ctx))

    async def end_model_call(self, ctx: CallContext, result: CallResult) -> EventResult:
        return await self._post("turn_end", self._fields("turn_end", ctx, result))

    async def agent_event(self, event: AgentEvent) -> EventResult:
        return await self._post("agent_event", self._fields("agent_event", event), retryable=True)

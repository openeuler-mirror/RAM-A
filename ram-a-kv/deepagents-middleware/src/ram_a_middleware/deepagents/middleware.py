"""DeepAgents hooks for RAM-A; model request IDs and KV values are borrowed."""
from __future__ import annotations

import logging
import time
from collections.abc import Mapping
from typing import Annotated, Any, Callable
from typing_extensions import NotRequired

from langchain.agents.middleware.types import AgentMiddleware, AgentState, ModelRequest, ModelResponse, PrivateStateAttr
from ram_a_middleware.sdk import AsyncRamAClient, RamAClient
from ram_a_middleware.sdk import traffic_log
from ram_a_middleware.sdk.events import AgentEvent, CallContext, CallResult, EventResult
from ram_a_middleware.sdk.protocol import _valid_hashes
from .identity import IdentityContext, normalize_session_id
from .model import cache_hashes_from_response, model_request_id, preserve_kv_response
from .serialization import json_safe

logger = logging.getLogger(__name__)


class RamAState(AgentState):
    _ram_a_turn_id: NotRequired[Annotated[str, PrivateStateAttr]]
    _ram_a_session_id: NotRequired[Annotated[str | None, PrivateStateAttr]]


class RamAKvMiddleware(AgentMiddleware):
    """Send agent/start and per-model cache events for explicitly named sessions.

    Standard ChatOpenAI replies retain KV fields automatically. Requests and
    upstream IDs are never modified. Complete snapshots are opt-in.
    """
    state_schema = RamAState

    def __init__(self, client: RamAClient, *, async_client: AsyncRamAClient | None = None,
                 identity: IdentityContext | None = None, response_max_bytes: int = 4096,
                 capture_debug: bool = False,
                 context_provider: Callable | None = None,
                 cache_identity_provider: Callable | None = None):
        if type(response_max_bytes) is not int or response_max_bytes < 2:
            raise ValueError("response_max_bytes must be an integer of at least 2")
        self.client = client
        self._owns_async_client = async_client is None
        self.async_client = async_client or AsyncRamAClient(
            client.daemon_url, timeout=client.timeout, auth_token=client.auth_token, retries=client.retries)
        self.identity = identity or IdentityContext()
        self.response_max_bytes = response_max_bytes
        self.capture_debug = capture_debug
        self.context_provider = context_provider
        self.cache_identity_provider = cache_identity_provider or cache_hashes_from_response

    async def aclose(self):
        """Close the async client created internally; supplied clients are borrowed."""
        if self._owns_async_client:
            await self.async_client.aclose()

    @staticmethod
    def _session_id_from_runtime(runtime: Any) -> str | None:
        context = getattr(runtime, "context", None)
        explicit = (context.get("session_id") if isinstance(context, Mapping)
                    else getattr(context, "session_id", None))
        normalized = normalize_session_id(explicit)
        if normalized is not None:
            return normalized
        try:
            from langgraph.config import get_config
            return normalize_session_id(get_config().get("configurable", {}).get("thread_id"))
        except RuntimeError:
            return None

    def _session_id(self, request):
        state = request.state or {}
        if "_ram_a_session_id" in state:
            return state["_ram_a_session_id"]
        return self._session_id_from_runtime(request.runtime)

    def _start(self, state, runtime):
        session = self._session_id_from_runtime(runtime)
        turn = self.identity.begin_turn(session) if session is not None else ""
        update = {"_ram_a_session_id": session, "_ram_a_turn_id": turn}
        if session is None:
            logger.warning("RAM-A disabled for this invocation: provide thread_id or context.session_id")
            return update, None
        return update, AgentEvent("agent", "start", session, agent_turn_id=turn,
                                  payload={"message_count": len(state.get("messages", []) or [])})

    @staticmethod
    def _observe(result: EventResult, session, request_id=None):
        if not result.ok or result.status != "success":
            # Do not log raw server errors, prompts, headers, or credentials.
            logger.warning("RAM-A event failed or degraded: type=%s session=%s request_id=%s event_id=%s code=%s",
                           result.event_type, session, request_id, result.event_id,
                           result.error_code or result.status)

    def before_agent(self, state: RamAState, runtime) -> dict:
        update, event = self._start(state, runtime)
        if event is not None:
            self._observe(self.client.agent_event(event), event.session_id)
        return update

    async def abefore_agent(self, state: RamAState, runtime) -> dict:
        update, event = self._start(state, runtime)
        if event is not None:
            self._observe(await self.async_client.agent_event(event), event.session_id)
        return update

    def _context(self, request, session):
        turn = (request.state or {}).get("_ram_a_turn_id") or self.identity.begin_turn(session)
        model = getattr(request.model, "model_name", request.model.__class__.__name__)
        ctx = CallContext(session, turn, model_request_id(request), model)
        if self.context_provider is not None:
            try:
                from dataclasses import replace
                supplied = self.context_provider(request, ctx)
                if supplied is not None:
                    if not isinstance(supplied, Mapping):
                        raise TypeError("context_provider must return a mapping")
                    ctx = replace(ctx, context=json_safe(supplied, max_bytes=self.response_max_bytes))
            except Exception:
                logger.warning("RAM-A context provider failed: session=%s request_id=%s", session, ctx.model_request_id)
        return ctx

    def _result(self, ctx, response, started):
        hashes = None
        try:
            hashes = self.cache_identity_provider(ctx, response)
            if hashes is not None and not _valid_hashes(hashes):
                raise ValueError("invalid KV identifiers")
        except Exception:
            hashes = None
            logger.warning("RAM-A KV extraction failed; reporting turn_end with null chunk_hashes: session=%s request_id=%s",
                           ctx.session_id, ctx.model_request_id)
        # Only cache-related metadata is sent by default. The SDK bounds the
        # combined debug_context, separately from the required cache identifiers.
        metadata = []
        for message in response.result:
            values = message.response_metadata
            metadata.append({key: values[key] for key in ("id", "kv_transfer_params") if key in values})
        return CallResult(total_time_ms=int((time.perf_counter() - started) * 1000),
                          message_count=len(response.result),
                          cache_hashes=tuple(hashes) if hashes is not None else None,
                          response=json_safe(response, max_bytes=self.response_max_bytes) if self.capture_debug else None,
                          response_metadata=json_safe({"messages": metadata}, max_bytes=self.response_max_bytes),
                          debug_max_bytes=self.response_max_bytes)

    def _log_model_request(self, session, ctx, request) -> None:
        """Log the deepagents-side model request body (opt-in via RAM_A_TRAFFIC_LOG)."""
        if not traffic_log.enabled():
            return
        messages = []
        try:
            for message in request.messages:
                content = message.content if isinstance(message.content, str) \
                    else json_safe(message.content, max_bytes=2048)
                messages.append({"role": getattr(message, "type", type(message).__name__),
                                 "content": content})
        except Exception:
            messages = None
        traffic_log.note("model_request", session_id=session,
                         model_request_id=ctx.model_request_id, model=ctx.model,
                         model_settings=json_safe(request.model_settings or {}, max_bytes=2048),
                         messages=messages)

    def _log_model_response(self, session, ctx, response) -> None:
        """Log a compact summary of the model response on the deepagents side."""
        if not traffic_log.enabled():
            return
        try:
            summaries = [{"role": getattr(m, "type", type(m).__name__),
                          "id": m.response_metadata.get("id"),
                          "has_kv_transfer_params": "kv_transfer_params" in m.response_metadata}
                         for m in response.result]
        except Exception:
            summaries = None
        traffic_log.note("model_response", session_id=session,
                         model_request_id=ctx.model_request_id, messages=summaries)

    def wrap_model_call(self, request: ModelRequest, handler: Callable) -> ModelResponse:
        session = self._session_id(request)
        if session is None:
            return handler(request)
        ctx = self._context(request, session)
        self._log_model_request(session, ctx, request)
        self._observe(self.client.begin_model_call(ctx), session, ctx.model_request_id)
        started = time.perf_counter()
        response = handler(preserve_kv_response(request))
        self._log_model_response(session, ctx, response)
        result = self._result(ctx, response, started)
        if result is not None:
            self._observe(self.client.end_model_call(ctx, result), session, ctx.model_request_id)
        return response

    async def awrap_model_call(self, request: ModelRequest, handler: Callable) -> ModelResponse:
        session = self._session_id(request)
        if session is None:
            return await handler(request)
        ctx = self._context(request, session)
        self._log_model_request(session, ctx, request)
        self._observe(await self.async_client.begin_model_call(ctx), session, ctx.model_request_id)
        started = time.perf_counter()
        response = await handler(preserve_kv_response(request))
        self._log_model_response(session, ctx, response)
        result = self._result(ctx, response, started)
        if result is not None:
            self._observe(await self.async_client.end_model_call(ctx, result), session, ctx.model_request_id)
        return response


class AsyncRamAKvMiddleware(RamAKvMiddleware):
    """Convenience entry point when the application owns only an async client."""
    def __init__(self, client: AsyncRamAClient, **kwargs):
        super().__init__(RamAClient(client.daemon_url, timeout=client.timeout,
                                   auth_token=client.auth_token, retries=client.retries),
                         async_client=client, **kwargs)

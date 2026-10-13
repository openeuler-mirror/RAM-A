"""DeepAgents hooks for RAM-A; model request IDs and KV values are borrowed."""
from __future__ import annotations

import json
import logging
import os
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
from .model import (cache_hashes_from_response, model_request_id, preserve_kv_response,
                    stream_timing_from_response, stream_usage_from_response, _extract_reasoning)
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
                 cache_identity_provider: Callable | None = None,
                 actrail_enabled: bool = False, actrail_endpoint_key: str | None = None,
                 actrail_agent_key: str | None = None,
                 actrail_model_deployment_key: str | None = None):
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
        # actrail-kv capture: opt-in via parameter or RAM_A_ACTRAIL_* env.
        self.actrail_enabled = actrail_enabled or os.getenv("RAM_A_ACTRAIL_ENABLED") in (
            "1", "true", "True", "yes")
        self.actrail_endpoint_key = actrail_endpoint_key \
            or os.getenv("RAM_A_ACTRAIL_ENDPOINT_KEY") or "chat"
        self.actrail_agent_key = actrail_agent_key \
            if actrail_agent_key is not None else os.getenv("RAM_A_ACTRAIL_AGENT_KEY")
        self.actrail_model_deployment_key = actrail_model_deployment_key \
            if actrail_model_deployment_key is not None \
            else os.getenv("RAM_A_ACTRAIL_MODEL_DEPLOYMENT_KEY")

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

    @staticmethod
    def _actrail_bytes(payload: Mapping) -> int:
        return len(json.dumps(payload, ensure_ascii=False, default=str).encode("utf-8"))

    def _actrail_oversized(self, payload: Mapping) -> bool:
        # Mirrors the actrail-kv receiver default body limit (8 MiB).
        return self._actrail_bytes(payload) > 8 * 1024 * 1024

    # 推理内容镜像保护上限：单个响应镜像文本超过则截断，避免撑爆 payload。
    _ACTRAIL_REASONING_MAX_BYTES = 16384

    @staticmethod
    def _response_extra(response) -> dict:
        """Collect response-side observations the analyzer does not consume.

        Returns ``{"finish_reason", "total_tokens", "model_name", "reasoning"?}``
        from the first result message, mirrored under ``payload._actrail_response``
        so engine-reported data is not dropped. Best-effort: any failure yields
        an empty dict (the KV turn_end flow is never affected).
        """
        if response is None:
            return {}
        try:
            for message in response.result:
                metadata = message.response_metadata or {}
                extra: dict = {}
                finish_reason = metadata.get("finish_reason")
                if finish_reason is not None:
                    extra["finish_reason"] = str(finish_reason)
                usage_metadata = getattr(message, "usage_metadata", None) or {}
                total_tokens = usage_metadata.get("total_tokens")
                if total_tokens is not None:
                    extra["total_tokens"] = int(total_tokens)
                model_name = (metadata.get("model_name")
                              or getattr(message, "model_name", None)
                              or metadata.get("model"))
                if model_name:
                    extra["model_name"] = str(model_name)
                reasoning = _extract_reasoning(getattr(message, "content", None))
                if reasoning:
                    if len(reasoning.encode("utf-8")) > RamAKvMiddleware._ACTRAIL_REASONING_MAX_BYTES:
                        reasoning = reasoning[:RamAKvMiddleware._ACTRAIL_REASONING_MAX_BYTES] + "…[truncated]"
                    extra["reasoning"] = reasoning
                return extra
        except Exception:
            return {}
        return {}

    def _build_openai_payload(self, request, model_name: str) -> dict:
        payload = {"model": model_name, "messages": []}
        try:
            for message in request.messages:
                content = message.content if isinstance(message.content, str) \
                    else json_safe(message.content, max_bytes=2048)
                payload["messages"].append({"role": getattr(message, "type", type(message).__name__),
                                            "content": content})
        except Exception:
            pass
        tools = getattr(request, "tools", None) or []
        if tools:
            payload["tools"] = json_safe(tools, max_bytes=8192)
        return payload

    def _build_actrail_capture(self, request, session, ctx, result,
                               usage: Mapping | None = None,
                               timing: tuple | None = None,
                               response=None) -> dict | None:
        """Build the turn_end actrail_capture (best-effort, never blocks).

        The payload mirrors the OpenAI request and carries the runtime metrics
        inside ``payload._actrail_response_usage`` (design doc): prompt_tokens /
        cached_tokens / completion_tokens from the engine usage, ttft_ms /
        total_ms measured on the streaming path. actrail-kv consumes
        prompt_tokens / cached_tokens / completion_tokens for the reported hit
        rate; ttft_ms / total_ms are stored verbatim for later dashboard
        display. Response-side observations actrail-kv does not consume
        (finish_reason / total_tokens / reasoning) are mirrored under the
        ``payload._actrail_response`` key so no engine-reported data is
        dropped; the analyzer ignores unknown top-level keys. An oversized or
        unserializable capture is discarded with a warning; KV turn_end is
        never affected.
        """
        if not self.actrail_enabled:
            return None
        if not self.actrail_endpoint_key:
            logger.warning("RAM-A actrail capture skipped, endpoint_key missing: session=%s", session)
            return None
        try:
            # 兜底：model_name 缺失时优先用 model_deployment_key（部署级标识），
            # 仍未配置时退回类名占位符（model 不参与投影单元，仅作标识）。
            model_name = (
                ctx.model
                or getattr(request.model, "model_name", None)
                or self.actrail_model_deployment_key
                or type(request.model).__name__
            )
            payload = self._build_openai_payload(request, model_name)
            response_usage: dict[str, int] = {}
            if usage is not None:
                if usage.get("input_tokens") is not None:
                    response_usage["prompt_tokens"] = usage["input_tokens"]
                if usage.get("output_tokens") is not None:
                    response_usage["completion_tokens"] = usage["output_tokens"]
                if usage.get("cached_tokens") is not None:
                    response_usage["cached_tokens"] = usage["cached_tokens"]
            if timing is not None:
                ttft_ms, total_ms = timing
                if ttft_ms is not None:
                    response_usage["ttft_ms"] = ttft_ms
                if total_ms is not None:
                    response_usage["total_ms"] = total_ms
            if response_usage:
                payload["_actrail_response_usage"] = response_usage
            response_extra = self._response_extra(response)
            if response_extra:
                payload["_actrail_response"] = response_extra
            if self._actrail_oversized(payload):
                logger.warning("RAM-A actrail capture skipped, payload too large: session=%s request_id=%s",
                               session, ctx.model_request_id)
                return None
            # Kept verbatim: the payload is already JSON-safe (plain str/int/
            # list/dict) and json_safe would redact values under "token" keys,
            # corrupting prompt_tokens/completion_tokens.
            return {"endpoint_key": self.actrail_endpoint_key,
                    "agent_key": self.actrail_agent_key,
                    "payload": payload}
        except Exception:
            logger.warning("RAM-A actrail capture failed: session=%s request_id=%s",
                           session, ctx.model_request_id)
            return None

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
            capture = self._build_actrail_capture(request, session, ctx, result,
                                                  usage=stream_usage_from_response(response),
                                                  timing=stream_timing_from_response(response),
                                                  response=response)
            self._observe(self.client.end_model_call(ctx, result, actrail_capture=capture),
                          session, ctx.model_request_id)
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
            capture = self._build_actrail_capture(request, session, ctx, result,
                                                  usage=stream_usage_from_response(response),
                                                  timing=stream_timing_from_response(response),
                                                  response=response)
            self._observe(await self.async_client.end_model_call(ctx, result, actrail_capture=capture),
                          session, ctx.model_request_id)
        return response


class AsyncRamAKvMiddleware(RamAKvMiddleware):
    """Convenience entry point when the application owns only an async client."""
    def __init__(self, client: AsyncRamAClient, **kwargs):
        super().__init__(RamAClient(client.daemon_url, timeout=client.timeout,
                                   auth_token=client.auth_token, retries=client.retries),
                         async_client=client, **kwargs)

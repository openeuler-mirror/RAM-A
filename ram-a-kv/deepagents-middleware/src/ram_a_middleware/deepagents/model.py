# Copyright (c) 2026 Huawei Technologies Co.,Ltd. All rights reserved.
#
# RAM-A is licensed under Mulan PSL v2.
# You can use this software according to the terms and conditions of the Mulan
# PSL v2.
# You may obtain a copy of Mulan PSL v2 at:
#         http://license.coscl.org.cn/MulanPSL2
# THIS SOFTWARE IS PROVIDED ON AN "AS IS" BASIS, WITHOUT WARRANTIES OF ANY
# KIND, EITHER EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO
# NON-INFRINGEMENT, MERCHANTABILITY OR FIT FOR A PARTICULAR PURPOSE.
# See the Mulan PSL v2 for more details.

"""Internal preservation of provider-owned KV fields, stream-based timing and usage."""
from __future__ import annotations

import time
from collections.abc import Mapping

from langchain_core.messages import AIMessage
from langchain_core.outputs import ChatGeneration, ChatGenerationChunk, ChatResult
from langchain_openai import ChatOpenAI


_CACHED_TOKEN_KEYS = ("cache_read", "cached_tokens")


def _deep_cached_tokens(source):
    """Depth-first search for a cache-read token count under arbitrary nesting."""
    if isinstance(source, Mapping):
        for key, raw in source.items():
            if key in _CACHED_TOKEN_KEYS and isinstance(raw, (int, float)):
                return int(raw)
        values = list(source.values())
    elif isinstance(source, (list, tuple)):
        values = list(source)
    else:
        return None
    for value in values:
        found = _deep_cached_tokens(value)
        if found is not None:
            return found
    return None


def _cached_tokens_from_message(message):
    """Extract cached (hit) input tokens from the engine usage.

    Shape-agnostic: depth-first searches ``usage_metadata`` first, then the
    engine's raw ``response_metadata.usage``, for a ``cache_read`` or
    ``cached_tokens`` count regardless of nesting (langchain's flat
    ``input_token_details.cache_read``, a nested ``cached_token_details``,
    or OpenAI/vLLM ``prompt_tokens_details.cached_tokens``). Returns None
    when the engine did not report cache hits, keeping "missing" distinct
    from a measured 0 (actrail-kv contract).
    """
    found = _deep_cached_tokens(getattr(message, "usage_metadata", None))
    if found is not None:
        return found
    response_metadata = getattr(message, "response_metadata", None) or {}
    return _deep_cached_tokens(response_metadata.get("usage"))


def _extract_reasoning(content):
    """Extract reasoning-block text from an AIMessage content list, e.g.
    ``[{'type': 'reasoning', 'reasoning': '...'}]`` (DeepSeek-style thinking)
    or plain text. Returns None when no reasoning content is present.
    """
    if isinstance(content, str):
        return None
    if isinstance(content, list):
        parts = []
        for block in content:
            if isinstance(block, Mapping) and block.get("type") == "reasoning":
                text = block.get("reasoning") or block.get("text") or ""
                if text:
                    parts.append(text)
        return "\n".join(parts) if parts else None
    return None


def _usage_from_message(message):
    """Extract input/output/cached token counts from a response message.

    Accepts langchain's ``usage_metadata``, falling back to the engine's raw
    ``response_metadata.usage`` when a model only mirrors the latter, plus the
    legacy ``prompt_tokens``/``completion_tokens`` aliases.
    """
    usage_metadata = getattr(message, "usage_metadata", None)
    if not isinstance(usage_metadata, Mapping):
        # 只镜像引擎原始 usage、不填 usage_metadata 的模型：回退原始形态。
        metadata = getattr(message, "response_metadata", None)
        engine_usage = metadata.get("usage") if isinstance(metadata, Mapping) else None
        usage_metadata = engine_usage if isinstance(engine_usage, Mapping) else None
    if isinstance(usage_metadata, Mapping):
        input_tokens = usage_metadata.get(
            "input_tokens", usage_metadata.get("prompt_tokens")
        )
        output_tokens = usage_metadata.get(
            "output_tokens", usage_metadata.get("completion_tokens")
        )
        if input_tokens is not None and output_tokens is not None:
            usage = {"input_tokens": input_tokens, "output_tokens": output_tokens}
            cached_tokens = _cached_tokens_from_message(message)
            if cached_tokens is not None:
                usage["cached_tokens"] = cached_tokens
            return usage
    return None


class _KvStreamTimingMixin:
    """Time model generation on every langchain invocation flavor.

    Engine-format independent: it works on any langchain ``BaseChatModel``
    subclass that implements ``_stream``/``_astream`` (ChatOpenAI,
    ChatAnthropic, third-party unified models, ...).

    langchain-core routes ``invoke``/``ainvoke`` through one of three paths
    inside ``_generate_with_cache`` / ``_agenerate_with_cache``:

    1. v2 protocol streaming (``_V2StreamingCallbackHandler`` attached) ->
       ``_aiter_v2_events`` drives ``_stream``/``_astream`` directly;
    2. v1 streaming (``_StreamingCallbackHandler`` attached, or
       ``streaming=True``) -> ``_stream``/``_astream`` directly;
    3. otherwise -> ``_generate``/``_agenerate``.

    The mixin therefore overrides all four entry points. ``_generate`` /
    ``_agenerate`` aggregate the stream and attach timing/usage through
    ``llm_output``; ``_stream``/``_astream`` stamp ``ram_a_timing`` /
    ``ram_a_usage`` onto the last chunk's ``generation_info``, which both the
    v1 path (``_gen_info_and_msg_metadata``) and the v2 bridge
    (``chunks_to_events``) fold into the assembled message's
    ``response_metadata``. Either way the middleware reads the same keys.

    Models without a streaming implementation keep their native path (the
    TTFT is then unavailable, but total latency is still measured by the
    outer middleware wrapper).
    """

    def _generate(self, messages, stop=None, run_manager=None, **kwargs):
        started = time.perf_counter()
        ttft_ms = None
        message = None
        last_chunk = None
        stream = getattr(self, "_stream", None)
        if stream is None:
            return super()._generate(
                messages, stop=stop, run_manager=run_manager, **kwargs
            )
        try:
            for chunk in stream(messages, stop=stop, run_manager=run_manager, **kwargs):
                if ttft_ms is None:
                    ttft_ms = int((time.perf_counter() - started) * 1000)
                last_chunk = chunk
                message = chunk.message if message is None else message + chunk.message
        except NotImplementedError:
            # 模型未实现流式（默认 _stream 抛 NotImplementedError）：回退原生路径。
            return super()._generate(
                messages, stop=stop, run_manager=run_manager, **kwargs
            )
        total_time = int((time.perf_counter() - started) * 1000)
        return self._timed_chat_result(message, last_chunk, ttft_ms, total_time)

    async def _agenerate(self, messages, stop=None, run_manager=None, **kwargs):
        started = time.perf_counter()
        ttft_ms = None
        message = None
        last_chunk = None
        astream = getattr(self, "_astream", None)
        if astream is None:
            return await super()._agenerate(
                messages, stop=stop, run_manager=run_manager, **kwargs
            )
        try:
            async for chunk in astream(
                messages, stop=stop, run_manager=run_manager, **kwargs
            ):
                if ttft_ms is None:
                    ttft_ms = int((time.perf_counter() - started) * 1000)
                last_chunk = chunk
                message = chunk.message if message is None else message + chunk.message
        except NotImplementedError:
            return await super()._agenerate(
                messages, stop=stop, run_manager=run_manager, **kwargs
            )
        total_time = int((time.perf_counter() - started) * 1000)
        return self._timed_chat_result(message, last_chunk, ttft_ms, total_time)

    def _timed_chat_result(self, message, last_chunk, ttft_ms, total_time):
        if message is None:
            message = AIMessage(content="")
        generation_info = {}
        if last_chunk is not None:
            generation_info = dict(last_chunk.generation_info or {})
        llm_output = {
            "model_provider": getattr(self, "model_provider", "generic"),
            "model_name": getattr(self, "model_name", None) or getattr(self, "model", None),
            "ram_a_timing": {"ttft_ms": ttft_ms, "total_time": total_time},
        }
        usage = _usage_from_message(message)
        if usage is not None:
            llm_output["ram_a_usage"] = usage
        return ChatResult(
            generations=[ChatGeneration(message=message, generation_info=generation_info)],
            llm_output=llm_output,
        )

    def _stamp_chunk(self, chunk, ttft_ms, total_time):
        """Attach ``ram_a_timing`` / ``ram_a_usage`` onto a chunk.

        langchain folds ``generation_info`` into ``message.response_metadata``
        on the v1 path and the v2 bridge, so stamping the (last) chunk makes
        ``stream_timing_from_response`` / ``stream_usage_from_response`` see
        the values no matter which invocation flavor langchain picked.
        """
        info = dict(chunk.generation_info or {})
        timing = {}
        if ttft_ms is not None:
            timing["ttft_ms"] = ttft_ms
        if total_time is not None:
            timing["total_time"] = total_time
        if timing:
            info["ram_a_timing"] = timing
        usage = _usage_from_message(chunk.message)
        if usage is not None:
            info["ram_a_usage"] = usage
        if info:
            chunk.generation_info = info
        return chunk

    def _stream(self, messages, stop=None, run_manager=None, **kwargs):
        """Time the v1/v2 streaming path (bypasses ``_generate``).

        Buffers one chunk so the last one can be stamped with the final
        total time; earlier chunks only carry the (already known) TTFT, and
        chunk accumulation lets the last chunk's values win in
        ``response_metadata``. Falls back to a single generated chunk when
        the provider never implemented streaming.
        """
        started = time.perf_counter()
        ttft_ms = None
        previous = None
        try:
            for chunk in super()._stream(
                messages, stop=stop, run_manager=run_manager, **kwargs
            ):
                if ttft_ms is None:
                    ttft_ms = int((time.perf_counter() - started) * 1000)
                if previous is not None:
                    yield self._stamp_chunk(previous, ttft_ms, None)
                previous = chunk
        except NotImplementedError:
            if previous is None:
                result = super()._generate(
                    messages, stop=stop, run_manager=run_manager, **kwargs
                )
                total_time = int((time.perf_counter() - started) * 1000)
                for gen in result.generations:
                    yield self._stamp_chunk(
                        ChatGenerationChunk(
                            message=gen.message, generation_info=gen.generation_info
                        ),
                        None,
                        total_time,
                    )
                return
            raise
        if previous is not None:
            total_time = int((time.perf_counter() - started) * 1000)
            yield self._stamp_chunk(previous, ttft_ms, total_time)

    async def _astream(self, messages, stop=None, run_manager=None, **kwargs):
        """Async twin of ``_stream``; see its docstring for the design."""
        started = time.perf_counter()
        ttft_ms = None
        previous = None
        try:
            async for chunk in super()._astream(
                messages, stop=stop, run_manager=run_manager, **kwargs
            ):
                if ttft_ms is None:
                    ttft_ms = int((time.perf_counter() - started) * 1000)
                if previous is not None:
                    yield self._stamp_chunk(previous, ttft_ms, None)
                previous = chunk
        except NotImplementedError:
            if previous is None:
                result = await super()._agenerate(
                    messages, stop=stop, run_manager=run_manager, **kwargs
                )
                total_time = int((time.perf_counter() - started) * 1000)
                for gen in result.generations:
                    yield self._stamp_chunk(
                        ChatGenerationChunk(
                            message=gen.message, generation_info=gen.generation_info
                        ),
                        None,
                        total_time,
                    )
                return
            raise
        if previous is not None:
            total_time = int((time.perf_counter() - started) * 1000)
            yield self._stamp_chunk(previous, ttft_ms, total_time)


class _KvResponseChatOpenAI(_KvStreamTimingMixin, ChatOpenAI):
    """ChatOpenAI subclass timed through the streaming path (see the mixin).

    Provider-owned KV fields (``kv_transfer_params``, top-level SSE fields such
    as ``ucm_block_ids``) are preserved on both response paths so the existing
    RAM-A KV coordination keeps working: the non-streaming path returns a whole
    ``ChatCompletion`` (``_create_chat_result``), while the streaming path
    converts chunk by chunk (``_convert_chunk_to_generation_chunk``) and the
    upstream converter ignores the field; the engine emits it on the final SSE
    chunk, so the merged ``AIMessageChunk`` then carries it into the final
    ``AIMessage.response_metadata``. ``stream_usage`` is enabled by
    ``preserve_kv_response`` so the engine reports usage in the final chunk.
    """

    @staticmethod
    def _field(chunk, key):
        """Read a top-level SSE field from a dict or an object chunk."""
        if isinstance(chunk, Mapping):
            return chunk.get(key)
        return getattr(chunk, key, None)

    def _create_chat_result(self, response, generation_info=None):
        """Non-streaming fallback: keep the whole ChatCompletion's KV fields.

        ``preserve_kv_response`` normally routes through streaming, but a model
        whose ``_stream`` is unavailable (or a direct non-streaming call) lands
        here, so the non-streaming response must still carry the field.
        """
        result = super()._create_chat_result(response, generation_info)
        transfer = self._field(response, "kv_transfer_params")
        if transfer is not None:
            for generation in result.generations:
                generation.message.response_metadata["kv_transfer_params"] = transfer
        return result

    def _convert_chunk_to_generation_chunk(self, chunk, default_chunk_class, base_generation_info):
        """Streaming path: copy top-level SSE fields onto each chunk's message."""
        generation = super()._convert_chunk_to_generation_chunk(
            chunk, default_chunk_class, base_generation_info
        )
        if generation is None:
            return None
        for key in ("kv_transfer_params", "metrics"):
            value = self._field(chunk, key)
            if value is not None:
                generation.message.response_metadata[key] = value
        return generation


def preserve_kv_response(request):
    """Adapt the model on a copy that times the response through streaming.

    ``ChatOpenAI`` is copied to ``_KvResponseChatOpenAI`` (also preserves
    provider-owned KV fields on stream chunks and enables
    ``stream_options.include_usage``). Any other langchain ``BaseChatModel``
    subclass (ChatAnthropic, third-party unified models, ...) is copied to a
    dynamically created subclass whose ``_generate``/``_agenerate`` run
    through the streaming path and attach ``ram_a_timing``/``ram_a_usage``,
    so TTFT/total latency and usage are captured regardless of the provider
    format. Models without a streaming implementation keep their native path
    (total latency still comes from the outer wrapper). No global monkey
    patch, request modification or new HTTP connection is involved.
    """
    model = request.model
    if type(model) is ChatOpenAI:
        model = _KvResponseChatOpenAI.model_construct(
            _fields_set=model.model_fields_set, **model.__dict__
        )
        # 引擎需 stream_options.include_usage 才会在流式末 chunk 返回 usage，
        # 否则流式响应不含 prompt/completion tokens（actrail 退化为 estimated）。
        model.stream_usage = True
        return request.override(model=model)
    if isinstance(model, _KvStreamTimingMixin):
        # 已适配过的模型（_KvResponseChatOpenAI 或复用适配实例）：
        # 直接沿用，避免对其再动态子类化破坏 MRO。
        return request.override(model=model)
    # 其余 pydantic BaseChatModel 子类：动态创建子类。需继承 mixin 而非拷贝
    # 方法，否则 mixin 内的 super() 闭包指向 _KvStreamTimingMixin，动态实例会
    # 触发 "super(type, obj): obj must be an instance or subtype of type"。
    timed_cls = type(
        f"_KvTimed{type(model).__name__}",
        (_KvStreamTimingMixin, type(model)),
        {},
    )
    model = timed_cls.model_construct(
        _fields_set=model.model_fields_set, **model.__dict__
    )
    # 声明了 stream_usage 的模型（如 ChatAnthropic）也强制开启，让引擎在流式
    # 末 chunk 回传 usage；未声明该字段的 pydantic 子类赋值会抛 ValueError。
    if "stream_usage" in type(model).model_fields:
        model.stream_usage = True
    return request.override(model=model)


def model_request_id(request):
    """Read the actual outgoing field; do not confuse graph run IDs with it."""
    settings = request.model_settings or {}
    # Per-call extra_body replaces model-level extra_body in ChatOpenAI.
    extra = settings.get("extra_body", getattr(request.model, "extra_body", None))
    value = extra.get("request_id") if isinstance(extra, Mapping) else None
    if value is None:
        value = settings.get("request_id")
    return value if isinstance(value, str) and value.strip() else None


def cache_hashes_from_response(_context, response):
    for message in response.result:
        transfer = message.response_metadata.get("kv_transfer_params")
        if transfer is not None:
            if not isinstance(transfer, Mapping):
                raise ValueError("kv_transfer_params must be an object")
            return transfer.get("ucm_block_ids")
    return None


def stream_timing_from_response(response):
    """Read ``ram_a_timing`` attached by ``_KvResponseChatOpenAI``.

    Returns ``(ttft_ms, total_time)``; either may be None when the timing is
    unavailable (e.g. a model class that could not be adapted).
    """
    for message in response.result:
        timing = message.response_metadata.get("ram_a_timing")
        if isinstance(timing, Mapping):
            ttft = timing.get("ttft_ms")
            total = timing.get("total_time")
            ttft_ms = int(ttft) if isinstance(ttft, (int, float)) and ttft > 0 else None
            total_time = int(total) if isinstance(total, (int, float)) and total > 0 else None
            return ttft_ms, total_time
    return None, None


def stream_usage_from_response(response):
    """Read usage from a model response, preferring our ``ram_a_usage`` stamp.

    Returns ``{"input_tokens", "output_tokens", "cached_tokens"?}`` or ``None``
    when the engine did not report usage; the actrail capture then omits the
    fields. ``cached_tokens`` is present only when the engine reported cache
    hits (missing kept distinct from 0).

    The stamp is only attached on the wrapped (timed) streaming path; a model
    that aggregates streaming internally carries usage in
    ``message.usage_metadata`` instead, so we fall back to
    ``_usage_from_message`` to keep the raw response analyzable.
    """
    for message in response.result:
        usage = message.response_metadata.get("ram_a_usage")
        if isinstance(usage, Mapping):
            input_tokens = usage.get("input_tokens")
            output_tokens = usage.get("output_tokens")
            if isinstance(input_tokens, (int, float)) and isinstance(output_tokens, (int, float)):
                result = {"input_tokens": int(input_tokens), "output_tokens": int(output_tokens)}
                cached = usage.get("cached_tokens")
                if isinstance(cached, (int, float)):
                    result["cached_tokens"] = int(cached)
                return result
        raw = _usage_from_message(message)
        if raw is not None:
            return raw
    return None

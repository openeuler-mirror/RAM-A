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

"""Stream metrics (usage + timing) extraction and the actrail-kv capture fields.

Covers:
  1. sync stream with usage -> ram_a_usage in response_metadata and the
     extractors return (ttft_ms, total_time) / {input_tokens, output_tokens};
  2. sync stream without usage -> ram_a_usage absent, extraction returns None
     (timing still present);
  3. middleware capture carries payload._actrail_response_usage.{prompt_tokens,
     completion_tokens, ttft_ms, total_ms} end-to-end through the daemon relay;
  4. middleware capture omits the fields when usage/timing are unavailable;
  5. SDK end_model_call carries actrail_capture in the turn_end body, omits
     it when None, and rejects a missing endpoint_key.
"""
from __future__ import annotations

import sys
import time
import unittest
from pathlib import Path
from unittest.mock import patch

TESTS_DIR = Path(__file__).resolve().parent
for _entry in (str(TESTS_DIR.parent / "src"), str(TESTS_DIR)):
    if _entry not in sys.path:
        sys.path.insert(0, _entry)

from langchain_core.messages import AIMessage, AIMessageChunk, HumanMessage
from langchain_core.outputs import ChatGenerationChunk

from ram_a_middleware.deepagents.model import _KvResponseChatOpenAI, stream_timing_from_response, stream_usage_from_response

from support import LoggingTransport, MockReceiver, TestDaemon, require, resolve_daemon_binary


def make_model() -> _KvResponseChatOpenAI:
    return _KvResponseChatOpenAI(model="deepseek", api_key="test")


def chunk_with_usage(content: str) -> ChatGenerationChunk:
    return ChatGenerationChunk(message=AIMessageChunk(
        content=content,
        usage_metadata={"input_tokens": 5, "output_tokens": 2, "total_tokens": 7},
    ))


def wait_for_relay(receiver, timeout: float = 5.0) -> None:
    """The daemon forwards actrail captures fire-and-forget (tokio::spawn)."""
    deadline = time.monotonic() + timeout
    while len(receiver.records) == 0 and time.monotonic() < deadline:
        time.sleep(0.05)


class StreamMetricsTests(unittest.TestCase):
    def test_sync_stream_usage_and_timing_extracted(self):
        model = make_model()
        chunks = [
            ChatGenerationChunk(message=AIMessageChunk(content="Hi")),
            chunk_with_usage(" there"),
        ]

        def slow_stream():
            for chunk in chunks:
                time.sleep(0.02)
                yield chunk

        with patch.object(model, "_stream", return_value=slow_stream()):
            message = model.invoke([HumanMessage(content="hello")])
        require(message.content == "Hi there", message.content)
        timing = message.response_metadata.get("ram_a_timing")
        require(isinstance(timing, dict) and "ttft_ms" in timing and "total_time" in timing,
                message.response_metadata)
        require(timing["total_time"] >= timing["ttft_ms"], timing)
        usage = message.response_metadata.get("ram_a_usage")
        require(usage == {"input_tokens": 5, "output_tokens": 2}, message.response_metadata)

        from langchain.agents.middleware.types import ModelResponse
        response = ModelResponse(result=[message])
        require(stream_usage_from_response(response) ==
                {"input_tokens": 5, "output_tokens": 2}, "usage extraction")
        ttft_ms, total_time = stream_timing_from_response(response)
        require(ttft_ms == timing["ttft_ms"] and total_time == timing["total_time"],
                (ttft_ms, total_time))

    def test_sync_stream_without_usage(self):
        model = make_model()
        chunks = [ChatGenerationChunk(message=AIMessageChunk(content="Hi"))]
        with patch.object(model, "_stream", return_value=iter(chunks)):
            message = model.invoke([HumanMessage(content="hello")])
        require("ram_a_usage" not in message.response_metadata, message.response_metadata)
        require("ram_a_timing" in message.response_metadata, message.response_metadata)
        from langchain.agents.middleware.types import ModelResponse
        require(stream_usage_from_response(ModelResponse(result=[message])) is None,
                "usage extraction must be None when the stream reported no usage")

    def test_async_stream_matches_sync(self):
        import asyncio

        model = make_model()
        chunks = [
            ChatGenerationChunk(message=AIMessageChunk(content="Hi")),
            chunk_with_usage(" there"),
        ]

        async def slow_astream():
            for chunk in chunks:
                await asyncio.sleep(0.02)
                yield chunk

        async def run():
            with patch.object(model, "_astream", return_value=slow_astream()):
                return await model.ainvoke([HumanMessage(content="hello")])

        message = asyncio.run(run())
        require(message.content == "Hi there", message.content)
        require(message.response_metadata["ram_a_usage"] ==
                {"input_tokens": 5, "output_tokens": 2}, message.response_metadata)
        require("ram_a_timing" in message.response_metadata, message.response_metadata)

    def test_actrail_model_deployment_key_fills_empty_model(self):
        from types import SimpleNamespace

        from langchain.agents.middleware.types import ModelRequest
        from langgraph.runtime import Runtime

        from ram_a_middleware.deepagents.middleware import RamAKvMiddleware
        from ram_a_middleware.sdk import RamAClient

        class NoNameModel:
            model_name = None

        client = RamAClient("http://127.0.0.1:9", timeout=0.5, retries=1)
        ctx = SimpleNamespace(model=None, model_request_id="r1")
        request = ModelRequest(
            model=NoNameModel(),
            messages=[HumanMessage(content="hi")],
            system_message=None,
            tools=[],
            state={"_ram_a_session_id": "s1", "_ram_a_turn_id": "t1"},
            runtime=Runtime(context={}),
        )

        mw = RamAKvMiddleware(client, actrail_enabled=True, actrail_endpoint_key="chat",
                              actrail_model_deployment_key="glm-5.2-bj")
        capture = mw._build_actrail_capture(request, "s1", ctx, None)
        require(capture["payload"]["model"] == "glm-5.2-bj", capture)

        mw2 = RamAKvMiddleware(client, actrail_enabled=True, actrail_endpoint_key="chat")
        capture2 = mw2._build_actrail_capture(request, "s1", ctx, None)
        require(capture2["payload"]["model"] == "NoNameModel", capture2)

    @unittest.skipUnless(resolve_daemon_binary(), "ram-a-kv daemon binary not found")
    def test_capture_carries_metrics_e2e(self):
        binary = resolve_daemon_binary()
        from langchain.agents.middleware.types import ModelRequest, ModelResponse
        from langgraph.runtime import Runtime

        from ram_a_middleware.deepagents.middleware import RamAKvMiddleware
        from ram_a_middleware.sdk import RamAClient

        receiver = None
        daemon = None
        try:
            receiver = MockReceiver()
            receiver_url = receiver.start()
            daemon = TestDaemon(binary, receiver_url)
            daemon.start()

            events: list[dict] = []
            client = RamAClient(daemon.url, timeout=2.0, retries=1,
                                transport=LoggingTransport(events))
            mw = RamAKvMiddleware(client, actrail_enabled=True,
                                  actrail_endpoint_key="chat",
                                  actrail_agent_key="metrics-agent")

            def handler(_request):
                message = AIMessage(
                    content="Hi",
                    response_metadata={
                        "ram_a_timing": {"ttft_ms": 5, "total_time": 9},
                        "ram_a_usage": {"input_tokens": 5, "output_tokens": 2},
                    },
                )
                return ModelResponse(result=[message])

            request = ModelRequest(
                model=make_model(),
                messages=[HumanMessage(content="hi")],
                system_message=None,
                tools=[],
                state={"_ram_a_session_id": "metrics-turn-end-001", "_ram_a_turn_id": "turn-1"},
                runtime=Runtime(context={}),
            )
            mw.wrap_model_call(request, handler)

            ends = [event for event in events if event.get("type") == "turn_end"]
            require(len(ends) == 1, events)
            capture = ends[0].get("actrail_capture")
            require(isinstance(capture, dict), ends[0])
            require(capture["endpoint_key"] == "chat", capture)
            require(capture["agent_key"] == "metrics-agent", capture)

            # The relay forwards fire-and-forget; wait for it to land.
            wait_for_relay(receiver)
            require(len(receiver.records) == 1, receiver.records)
            payload = receiver.records[0]
            require(payload.get("model") == "deepseek", payload)
            require(payload.get("_actrail_response_usage") ==
                    {"prompt_tokens": 5, "completion_tokens": 2, "ttft_ms": 5, "total_ms": 9},
                    payload)
            client.close()
        finally:
            if daemon is not None:
                daemon.stop()
            if receiver is not None:
                receiver.stop()

    @unittest.skipUnless(resolve_daemon_binary(), "ram-a-kv daemon binary not found")
    def test_capture_omits_metrics_when_absent_e2e(self):
        binary = resolve_daemon_binary()
        from langchain.agents.middleware.types import ModelRequest, ModelResponse
        from langgraph.runtime import Runtime

        from ram_a_middleware.deepagents.middleware import RamAKvMiddleware
        from ram_a_middleware.sdk import RamAClient

        receiver = None
        daemon = None
        try:
            receiver = MockReceiver()
            receiver_url = receiver.start()
            daemon = TestDaemon(binary, receiver_url)
            daemon.start()

            events: list[dict] = []
            client = RamAClient(daemon.url, timeout=2.0, retries=1,
                                transport=LoggingTransport(events))
            mw = RamAKvMiddleware(client, actrail_enabled=True,
                                  actrail_endpoint_key="chat")

            def handler(_request):
                return ModelResponse(result=[AIMessage(content="Hi")])

            request = ModelRequest(
                model=make_model(),
                messages=[HumanMessage(content="hi")],
                system_message=None,
                tools=[],
                state={"_ram_a_session_id": "metrics-absent-001", "_ram_a_turn_id": "turn-1"},
                runtime=Runtime(context={}),
            )
            mw.wrap_model_call(request, handler)

            ends = [event for event in events if event.get("type") == "turn_end"]
            require(len(ends) == 1, events)
            # The relay forwards fire-and-forget; wait for it to land.
            wait_for_relay(receiver)
            require(len(receiver.records) == 1, receiver.records)
            payload = receiver.records[0]
            require("_actrail_response_usage" not in payload, payload)
            client.close()
        finally:
            if daemon is not None:
                daemon.stop()
            if receiver is not None:
                receiver.stop()

    def test_end_model_call_carries_capture_in_turn_end_body(self):
        from langchain.agents.middleware.types import ModelRequest, ModelResponse
        from langgraph.runtime import Runtime

        from ram_a_middleware.deepagents.middleware import RamAKvMiddleware
        from ram_a_middleware.sdk import RamAClient

        events: list[dict] = []
        client = RamAClient("http://127.0.0.1:1", timeout=0.2, retries=0,
                            transport=LoggingTransport(events))
        mw = RamAKvMiddleware(client, actrail_enabled=True,
                              actrail_endpoint_key="chat")

        def handler(_request):
            message = AIMessage(
                content="Hi",
                response_metadata={
                    "ram_a_timing": {"ttft_ms": 5, "total_time": 9},
                    "ram_a_usage": {"input_tokens": 5, "output_tokens": 2},
                },
            )
            return ModelResponse(result=[message])

        request = ModelRequest(
            model=make_model(),
            messages=[HumanMessage(content="hi")],
            system_message=None,
            tools=[],
            state={"_ram_a_session_id": "proto-001", "_ram_a_turn_id": "turn-1"},
            runtime=Runtime(context={}),
        )
        mw.wrap_model_call(request, handler)
        client.close()

        ends = [event for event in events if event.get("type") == "turn_end"]
        require(len(ends) == 1, events)
        capture = ends[0].get("actrail_capture")
        require(isinstance(capture, dict), ends[0])
        require(capture["endpoint_key"] == "chat", capture)
        require(capture["payload"]["_actrail_response_usage"]["prompt_tokens"] == 5, capture)

    def test_end_model_call_omits_capture_when_disabled(self):
        from langchain.agents.middleware.types import ModelRequest, ModelResponse
        from langgraph.runtime import Runtime

        from ram_a_middleware.deepagents.middleware import RamAKvMiddleware
        from ram_a_middleware.sdk import RamAClient

        events: list[dict] = []
        client = RamAClient("http://127.0.0.1:1", timeout=0.2, retries=0,
                            transport=LoggingTransport(events))
        mw = RamAKvMiddleware(client, actrail_enabled=False)

        def handler(_request):
            return ModelResponse(result=[AIMessage(content="Hi")])

        request = ModelRequest(
            model=make_model(),
            messages=[HumanMessage(content="hi")],
            system_message=None,
            tools=[],
            state={"_ram_a_session_id": "proto-off-001", "_ram_a_turn_id": "turn-1"},
            runtime=Runtime(context={}),
        )
        mw.wrap_model_call(request, handler)
        client.close()

        ends = [event for event in events if event.get("type") == "turn_end"]
        require(len(ends) == 1, events)
        require("actrail_capture" not in ends[0], ends[0])

    def test_invalid_capture_rejected_at_sdk_boundary(self):
        from ram_a_middleware.sdk.events import CallContext, CallResult
        from ram_a_middleware.sdk.protocol import RamAProtocolClient

        events: list[dict] = []
        client = RamAProtocolClient("http://127.0.0.1:1", timeout=0.2, retries=0,
                                   transport=LoggingTransport(events))
        result = client.end_model_call(
            CallContext(session_id="s", agent_turn_id="t"),
            CallResult(),
            actrail_capture={"endpoint_key": "  ", "payload": {}},
        )
        client.close()
        require(not result.ok, result)
        require(result.error_code == "invalid_turn_end", result)
        # The invalid capture is dropped without sending anything.
        require(not any(event.get("type") == "turn_end" for event in events), events)


if __name__ == "__main__":
    unittest.main()

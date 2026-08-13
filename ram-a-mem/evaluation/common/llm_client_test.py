from __future__ import annotations

import http.client
import urllib.error

import pytest

from common.llm_client import OpenAICompatibleClient


class _Response:
    def __init__(self, body: bytes) -> None:
        self.body = body

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False

    def read(self) -> bytes:
        return self.body


def test_chat_retries_incomplete_chunked_response(monkeypatch) -> None:
    monkeypatch.setenv("TEST_LLM_KEY", "secret")
    attempts = iter(
        [
            http.client.IncompleteRead(b"partial", 10),
            _Response(
                b'{"choices":[{"message":{"content":"ok"}}],'
                b'"usage":{"total_tokens":3,"prompt_tokens":2,"completion_tokens":1}}'
            ),
        ]
    )
    calls = []

    def fake_urlopen(*args, **kwargs):
        calls.append((args, kwargs))
        value = next(attempts)
        if isinstance(value, BaseException):
            raise value
        return value

    monkeypatch.setattr("common.llm_client.urllib.request.urlopen", fake_urlopen)
    monkeypatch.setattr("common.llm_client.time.sleep", lambda _: None)
    client = OpenAICompatibleClient("TEST_LLM_KEY", max_retries=2)

    result = client.chat("model", [{"role": "user", "content": "hello"}])

    assert result.content == "ok"
    assert len(calls) == 2


def test_chat_does_not_retry_non_retryable_http_status(monkeypatch) -> None:
    monkeypatch.setenv("TEST_LLM_KEY", "secret")
    calls = []

    def bad_request(*args, **kwargs):
        calls.append((args, kwargs))
        raise urllib.error.HTTPError(
            "https://example.test/chat/completions",
            400,
            "bad request",
            {},
            None,
        )

    monkeypatch.setattr("common.llm_client.urllib.request.urlopen", bad_request)
    monkeypatch.setattr("common.llm_client.time.sleep", lambda _: None)
    client = OpenAICompatibleClient("TEST_LLM_KEY", max_retries=8)

    with pytest.raises(RuntimeError, match="HTTP Error 400"):
        client.chat("model", [{"role": "user", "content": "hello"}])

    assert len(calls) == 1

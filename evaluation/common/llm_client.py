"""Small OpenAI-compatible chat client used by evaluation scripts."""

import json
import http.client
import os
import socket
import time
import urllib.error
import urllib.request
from dataclasses import dataclass


@dataclass
class ChatResult:
    content: str
    latency_ms: float
    prompt_tokens: int
    completion_tokens: int
    total_tokens: int
    raw: dict


class OpenAICompatibleClient:
    """Minimal client for OpenAI-compatible /chat/completions endpoints."""

    def __init__(
        self,
        api_key_env: str,
        base_url: str = "https://openrouter.ai/api/v1",
        timeout_s: int = 120,
        max_retries: int = 3,
        thinking: str | None = None,
    ) -> None:
        api_key = os.getenv(api_key_env)
        if not api_key:
            raise RuntimeError(f"missing API key env {api_key_env}")
        self.api_key = api_key
        self.base_url = base_url.rstrip("/")
        self.timeout_s = timeout_s
        self.max_retries = max_retries
        self.thinking = thinking

    def chat(
        self,
        model: str,
        messages: list[dict],
        temperature: float = 0.0,
        max_tokens: int = 512,
    ) -> ChatResult:
        payload = {
            "model": model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
        }
        if self.thinking:
            # Provider-specific extension used by GLM-5/5.1 compatible endpoints.
            # Leave unset for standard OpenAI/OpenRouter models.
            payload["thinking"] = {"type": self.thinking}
        body = json.dumps(payload).encode("utf-8")
        headers = {
            "Authorization": f"Bearer {self.api_key}",
            "Content-Type": "application/json",
        }

        last_error = None
        for attempt in range(self.max_retries):
            started = time.monotonic()
            request = urllib.request.Request(
                f"{self.base_url}/chat/completions",
                data=body,
                headers=headers,
                method="POST",
            )
            try:
                with urllib.request.urlopen(request, timeout=self.timeout_s) as response:
                    raw = json.loads(response.read().decode("utf-8"))
                latency_ms = (time.monotonic() - started) * 1000
                content = raw["choices"][0]["message"].get("content") or ""
                if not content.strip():
                    last_error = RuntimeError("chat completion returned empty content")
                    if attempt + 1 == self.max_retries:
                        break
                    time.sleep(2**attempt)
                    continue
                usage = raw.get("usage") or {}
                prompt_tokens = usage.get("prompt_tokens") or _estimate_tokens(messages)
                completion_tokens = usage.get("completion_tokens") or _estimate_tokens(content)
                total_tokens = usage.get("total_tokens") or prompt_tokens + completion_tokens
                return ChatResult(
                    content=content.strip(),
                    latency_ms=latency_ms,
                    prompt_tokens=int(prompt_tokens),
                    completion_tokens=int(completion_tokens),
                    total_tokens=int(total_tokens),
                    raw=raw,
                )
            except (
                urllib.error.HTTPError,
                urllib.error.URLError,
                TimeoutError,
                http.client.RemoteDisconnected,
                ConnectionResetError,
                socket.timeout,
            ) as error:
                last_error = error
                if attempt + 1 == self.max_retries:
                    break
                time.sleep(2**attempt)

        raise RuntimeError(f"chat completion failed after retries: {last_error}")


def _estimate_tokens(value: object) -> int:
    """Cheap fallback when the provider omits usage."""
    text = json.dumps(value, ensure_ascii=False) if not isinstance(value, str) else value
    return max(1, len(text) // 4)

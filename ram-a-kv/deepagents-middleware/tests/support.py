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

"""Shared harness for the deepagents-middleware regression tests.

Provides a mock actrail-kv receiver and an isolated RAM-A daemon runner, plus
small helpers for scripted streams and request capture.
"""
from __future__ import annotations

import json
import os
import re
import socket
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

import httpx
from langchain_core.messages import AIMessageChunk
from langchain_core.outputs import ChatGenerationChunk

REPO_ROOT = Path(__file__).resolve().parents[1]  # deepagents-middleware/
DAEMON_CANDIDATES = (
    Path(os.environ.get("RAM_A_KV_DAEMON_BIN") or ""),
    REPO_ROOT.parent / "target" / "release" / "ram-a-kv",
    REPO_ROOT.parent / "target" / "debug" / "ram-a-kv",
)


def require(condition: bool, message: object) -> None:
    if not condition:
        raise AssertionError(message)


def resolve_daemon_binary() -> Path | None:
    for candidate in DAEMON_CANDIDATES:
        if candidate and candidate.is_file():
            return candidate
    return None


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def stream_chunk(content: str, *, kv: dict | None = None, usage: dict | None = None) -> ChatGenerationChunk:
    """One scripted SSE-equivalent chunk with optional usage / top-level KV fields."""
    metadata: dict[str, Any] = {}
    if usage is not None:
        metadata["usage"] = usage
    message = AIMessageChunk(content=content, response_metadata=metadata)
    if kv is not None:
        message.response_metadata.update(kv)
    return ChatGenerationChunk(message=message)


class LoggingTransport(httpx.HTTPTransport):
    """Record JSON request bodies while forwarding to the real daemon."""

    def __init__(self, events: list[dict[str, Any]]) -> None:
        super().__init__()
        self.events = events

    def handle_request(self, request: httpx.Request) -> httpx.Response:
        self.events.append(json.loads(request.content or b"{}"))
        return super().handle_request(request)


class MockReceiver:
    """actrail-kv receiver stand-in: POST /requests collects the payload."""

    def __init__(self) -> None:
        self.records: list[dict[str, Any]] = []
        self.headers: list[dict[str, str]] = []

        def do_post(handler):
            length = int(handler.headers.get("Content-Length", 0))
            body = handler.rfile.read(length)
            self.records.append(json.loads(body))
            self.headers.append({k: v for k, v in handler.headers.items()
                                 if k.lower().startswith("x-actrail-")})
            payload = json.dumps({"ok": True}).encode()
            handler.send_response(200)
            handler.send_header("Content-Type", "application/json")
            handler.send_header("Content-Length", str(len(payload)))
            handler.end_headers()
            handler.wfile.write(payload)
            handler.wfile.flush()

        def do_get(handler):
            payload = json.dumps({"ok": True}).encode()
            handler.send_response(200)
            handler.send_header("Content-Length", str(len(payload)))
            handler.end_headers()
            handler.wfile.write(payload)
            handler.wfile.flush()

        def log_message(self, *_args):
            pass

        self.handler_type = type("Handler", (BaseHTTPRequestHandler,), {
            "log_message": log_message,
            "do_POST": do_post,
            "do_GET": do_get,
        })
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), self.handler_type)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def start(self) -> str:
        self.thread.start()
        return f"http://127.0.0.1:{self.server.server_address[1]}"

    def stop(self) -> None:
        self.server.shutdown()
        self.server.server_close()


class TestDaemon:
    """Run RAM-A in an isolated temp dir with actrail relay to a receiver."""

    def __init__(self, binary: Path, receiver_url: str) -> None:
        self.binary = binary
        self.tmp = tempfile.TemporaryDirectory(prefix="ram-a-middleware-test-")
        self.tmpdir = Path(self.tmp.name)
        self.port = free_port()
        self.url = f"http://127.0.0.1:{self.port}"
        self.log_path = self.tmpdir / "daemon.log"
        self.capture_path = self.tmpdir / "received-events.jsonl"
        self.proc: subprocess.Popen[bytes] | None = None
        self.log_handle: Any = None
        config = "\n".join((
            f'listen_addr = "127.0.0.1:{self.port}"',
            "noop_backend = true",
            f'session_store_path = "{self.tmpdir / "sessions.db"}"',
            f'test_capture_path = "{self.capture_path}"',
            f'actrail_receiver_url = "{receiver_url}"',
            "actrail_model_deployment_key = \"deepseek\"",
            "actrail_kv_namespace = \"default\"",
            "",
        ))
        (self.tmpdir / "config.toml").write_text(config, encoding="utf-8")

    def start(self) -> None:
        env = dict(os.environ)
        env["RAM_A_KV_CONFIG"] = str(self.tmpdir / "config.toml")
        self.log_handle = self.log_path.open("wb")
        self.proc = subprocess.Popen(
            [str(self.binary)], cwd=self.tmpdir, env=env,
            stdout=self.log_handle, stderr=subprocess.STDOUT,
        )
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(f"daemon exited early:\n{self.log()}")
            try:
                response = httpx.post(f"{self.url}/event", json={"type": "health"}, timeout=0.5)
                if response.status_code == 200 and response.json().get("ok"):
                    return
            except Exception:
                pass
            time.sleep(0.1)
        raise RuntimeError(f"daemon did not become healthy:\n{self.log()}")

    def log(self) -> str:
        if not self.log_path.exists():
            return ""
        return re.sub(r"\x1b\[[0-9;]*m", "", self.log_path.read_text(errors="replace"))

    def capture_records(self) -> list[dict[str, Any]]:
        if not self.capture_path.is_file():
            return []
        records: list[dict[str, Any]] = []
        for line in self.capture_path.read_text(encoding="utf-8").splitlines():
            if line.strip():
                records.append(json.loads(line))
        return records

    def stop(self) -> None:
        proc, self.proc = self.proc, None
        if proc is not None and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=5)

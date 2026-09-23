"""Opt-in traffic logging for RAM-A events, configured via environment variables.

Intended for on-site verification in customer environments: set
RAM_A_TRAFFIC_LOG to an absolute file path and every event sent to the
RAM-A daemon (request body and daemon acknowledgement) is appended there
as one JSON line per record. Logging is disabled unless that variable is
set to a non-empty value, and it never affects event delivery: all
failures inside this module are swallowed.

Environment variables:
  RAM_A_TRAFFIC_LOG        log file path; logging disabled when unset/empty
  RAM_A_TRAFFIC_MAX_BYTES  per-record body truncation limit (default 65536)
  RAM_A_TRAFFIC_STDERR     set to "1" to also mirror records to stderr
"""
from __future__ import annotations

import json
import logging
import os
import sys
import threading
import time

_LOGGER_NAME = "ram_a_middleware.traffic"
_lock = threading.Lock()
_logger = None
_path = None
_max_bytes = 65536
_stderr = False


def _configure() -> None:
    global _logger, _path, _max_bytes, _stderr
    path = os.environ.get("RAM_A_TRAFFIC_LOG", "") or ""
    if not path:
        return
    try:
        max_bytes = int(os.environ.get("RAM_A_TRAFFIC_MAX_BYTES", "65536"))
    except ValueError:
        max_bytes = 65536
    stderr = os.environ.get("RAM_A_TRAFFIC_STDERR", "") == "1"
    directory = os.path.dirname(os.path.abspath(path))
    if directory and not os.path.isdir(directory):
        return  # refuse to create directories silently; stay disabled
    logger = logging.getLogger(_LOGGER_NAME)
    logger.setLevel(logging.INFO)
    logger.propagate = False
    if not any(getattr(h, "_ram_a_traffic_path", None) == os.path.abspath(path)
               for h in logger.handlers):
        handler = logging.FileHandler(path, mode="a", encoding="utf-8")
        handler.setFormatter(logging.Formatter("%(message)s"))
        handler._ram_a_traffic_path = os.path.abspath(path)
        logger.addHandler(handler)
    _logger, _path, _max_bytes, _stderr = logger, path, max_bytes, stderr


def _record(record: dict) -> None:
    global _logger, _path
    if _logger is None or (_path or "") != (os.environ.get("RAM_A_TRAFFIC_LOG", "") or ""):
        # (Re)resolve configuration lazily so tests can change the env var.
        with _lock:
            _configure()
        if _logger is None:
            return
    line = json.dumps(record, ensure_ascii=False, default=str)
    if len(line.encode("utf-8", "replace")) > _max_bytes:
        line = json.dumps({"truncated": True, "bytes": len(line.encode("utf-8", "replace")),
                           "preview": line[:_max_bytes]}, ensure_ascii=False)
    try:
        _logger.info(line)
        if _stderr:
            print(line, file=sys.stderr)
    except Exception:
        pass  # logging must never break event delivery


def enabled() -> bool:
    """Whether traffic logging is switched on via RAM_A_TRAFFIC_LOG."""
    return bool(os.environ.get("RAM_A_TRAFFIC_LOG"))


def note(kind: str, **fields) -> None:
    """Log a deepagents-side observation (e.g. the model request body).

    Records use "dir": "deepagents" so they can be told apart from the
    middleware -> RAM-A request/response traffic.
    """
    if not enabled():
        return
    record = {"ts": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "dir": "deepagents", "type": kind}
    record.update(fields)
    try:
        _record(record)
    except Exception:
        pass


def request(url: str, body: dict) -> None:
    """Log the exact JSON body about to be POSTed to the RAM-A daemon."""
    if not os.environ.get("RAM_A_TRAFFIC_LOG"):
        return
    try:
        _record({"ts": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "dir": "request",
                 "url": url, "body": body})
    except Exception:
        pass


def response(kind: str, event_id, result) -> None:
    """Log the daemon acknowledgement for one delivered event."""
    if not os.environ.get("RAM_A_TRAFFIC_LOG"):
        return
    try:
        _record({"ts": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "dir": "response",
                 "type": kind, "event_id": getattr(result, "event_id", event_id) or event_id,
                 "ok": getattr(result, "ok", None), "status": getattr(result, "status", None),
                 "error_code": getattr(result, "error_code", None),
                 "error": getattr(result, "error", None)})
    except Exception:
        pass

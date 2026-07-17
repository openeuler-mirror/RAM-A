"""Deterministic serialization, hashing, and token estimation helpers."""

from __future__ import annotations

import hashlib
import json
import re
from typing import Any


_TOKEN_RE = re.compile(
    r"[A-Za-z0-9]+|[\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff]|[^\s]",
    re.UNICODE,
)


def canonical_json(value: Any) -> str:
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    )


def stable_hash(*values: Any) -> str:
    payload = canonical_json(values).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()[:24]


def estimate_tokens(text: str) -> int:
    return len(_TOKEN_RE.findall(text))

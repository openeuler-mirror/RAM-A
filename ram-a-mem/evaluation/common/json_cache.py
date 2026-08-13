"""Versioned, deterministic JSON cache for evaluation-side model calls."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import re
from typing import Any, Sequence


_SAFE_NAMESPACE_RE = re.compile(r"^[A-Za-z0-9_.-]+$")


class CacheCorruptionError(RuntimeError):
    """Raised when an existing cache entry is not valid JSON."""


class JsonCache:
    def __init__(self, root: Path, version: str = "cache_v1") -> None:
        self.root = Path(root)
        self.version = version

    def get(self, namespace: str, key_parts: Sequence[Any]) -> Any | None:
        path = self._path(namespace, key_parts)
        if not path.exists():
            return None
        try:
            return json.loads(path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, UnicodeDecodeError) as error:
            raise CacheCorruptionError(f"corrupt cache entry: {path}") from error

    def put(self, namespace: str, key_parts: Sequence[Any], value: Any) -> Path:
        path = self._path(namespace, key_parts)
        path.parent.mkdir(parents=True, exist_ok=True)
        temporary = path.with_suffix(path.suffix + ".tmp")
        temporary.write_text(
            json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")),
            encoding="utf-8",
        )
        temporary.replace(path)
        return path

    def _path(self, namespace: str, key_parts: Sequence[Any]) -> Path:
        if not _SAFE_NAMESPACE_RE.fullmatch(namespace):
            raise ValueError(f"unsafe cache namespace: {namespace!r}")
        digest = _stable_hash(self.version, list(key_parts))
        return self.root / namespace / f"{digest}.json"


def _stable_hash(*values: Any) -> str:
    payload = json.dumps(
        values, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()[:24]

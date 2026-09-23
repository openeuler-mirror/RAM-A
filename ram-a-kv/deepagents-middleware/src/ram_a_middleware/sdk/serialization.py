"""Best-effort JSON snapshots that cannot interrupt a model response."""
from __future__ import annotations

import json
import math
from collections.abc import Mapping, Sequence, Set
from dataclasses import fields, is_dataclass
from enum import Enum
from typing import Any


def json_safe(value: Any, *, max_bytes: int | None = None) -> dict[str, Any]:
    """Return a JSON object; unsupported values become diagnostic markers.

    A positive byte limit (at least 2) bounds the compact ASCII JSON encoding.
    Small limits retain only a truncation marker, or an empty object.
    """
    if max_bytes is not None and (type(max_bytes) is not int or max_bytes < 2):
        raise ValueError("max_bytes must be an integer of at least 2")
    seen: set[int] = set()

    def redact(key, value):
        name = str(key).lower().replace("-", "_")
        if any(word in name for word in ("password", "secret", "token", "authorization", "api_key", "cookie")):
            return "[redacted]"
        return convert(value)

    def convert(item: Any) -> Any:
        if isinstance(item, Enum):
            return convert(item.value)
        if isinstance(item, float) and not math.isfinite(item):
            return str(item)
        if item is None or isinstance(item, (str, int, float, bool)):
            return item
        ident = id(item)
        if ident in seen:
            return {"__ram_a_type__": "cycle"}
        seen.add(ident)
        try:
            if isinstance(item, Mapping):
                return {str(convert(k)): redact(k, v) for k, v in item.items()}
            if is_dataclass(item) and not isinstance(item, type):
                return {f.name: redact(f.name, getattr(item, f.name)) for f in fields(item)}
            if callable(getattr(item, "model_dump", None)):
                try:
                    return convert(item.model_dump(mode="json"))
                except Exception:
                    return convert(item.model_dump())
            if isinstance(item, (Sequence, Set)) and not isinstance(item, (str, bytes, bytearray)):
                return [convert(v) for v in item]
            if hasattr(item, "__dict__"):
                return {str(k): redact(k, v) for k, v in vars(item).items() if not str(k).startswith("_")}
            return {"__ram_a_type__": "unsupported", "type": type(item).__name__}
        except Exception:
            return {"__ram_a_type__": "unserializable"}
        finally:
            seen.discard(ident)

    try:
        snapshot = convert(value)
        if not isinstance(snapshot, dict):
            snapshot = {"result": snapshot}
        encoded = json.dumps(snapshot, ensure_ascii=True, separators=(",", ":"), allow_nan=False).encode()
    except Exception:
        snapshot = {"__ram_a_type__": "unserializable"}
        encoded = json.dumps(snapshot, separators=(",", ":")).encode()
    if max_bytes is not None and max_bytes > 0 and len(encoded) > max_bytes:
        snapshot = {"truncated": True, "original_bytes": len(encoded)}
        if len(json.dumps(snapshot, separators=(",", ":")).encode()) > max_bytes:
            snapshot = {"truncated": True} if max_bytes >= 18 else {}
    return snapshot

"""Shared helpers for evaluation output directories and metadata."""

from __future__ import annotations

import json
import os
import re
import subprocess
from datetime import datetime
from pathlib import Path
from typing import Any

from common.config import OUTPUTS_DIR


def timestamp_run_id() -> str:
    return datetime.now().strftime("%Y-%m-%dT%H%M%S%f")


def default_run_dir(dataset: str, run_id: str | None = None) -> Path:
    dataset_slug = safe_slug(dataset)
    run_slug = safe_slug(run_id or timestamp_run_id())
    return Path(OUTPUTS_DIR) / dataset_slug / run_slug


def safe_slug(value: str) -> str:
    value = str(value)
    if not re.fullmatch(r"[A-Za-z0-9_.-]+", value):
        raise ValueError(f"unsafe run artifact path component: {value!r}")
    if value in {".", ".."}:
        raise ValueError(f"unsafe run artifact path component: {value!r}")
    return value


def git_hash() -> str:
    try:
        result = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            capture_output=True,
            text=True,
        )
    except Exception:
        return "unknown"
    return result.stdout.strip() if result.returncode == 0 else "unknown"


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")


def write_run_meta(path: Path, **fields: Any) -> dict[str, Any]:
    meta = {
        "created_at": datetime.now().isoformat(timespec="seconds"),
        "git_hash": git_hash(),
    }
    meta.update({key: value for key, value in fields.items() if value is not None})
    write_json(path, meta)
    return meta


def ensure_dir(path: Path) -> Path:
    os.makedirs(path, exist_ok=True)
    return path

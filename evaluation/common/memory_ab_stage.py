"""Resumable stage execution for memory A/B evaluation pipelines."""

from __future__ import annotations

from datetime import datetime, timezone
import json
import os
from pathlib import Path
import shlex
import subprocess
import time
from typing import Any, Callable

from common.memory_ab import canonical_sha256, file_sha256


EVALUATION_ROOT = Path(__file__).resolve().parents[1]


def run_stage(
    name: str,
    command: list[str],
    outputs: tuple[Path, ...],
    manifest: dict[str, Any],
    *,
    inputs: tuple[Path, ...] = (),
    clean_outputs_on_rerun: bool = False,
    env_overrides: dict[str, str] | None = None,
    runner: Callable[..., subprocess.CompletedProcess] = subprocess.run,
) -> None:
    """Run a stage unless its command, inputs, and outputs still match."""
    if not outputs:
        raise ValueError(f"stage {name} must declare at least one output")
    outputs = tuple(Path(path) for path in outputs)
    complete_path = outputs[0].parent / "stages" / f"{name}.complete.json"
    expected = dict(manifest)
    expected["stage"] = name
    expected["command_hash"] = canonical_sha256(command)
    missing_inputs = [str(path) for path in inputs if not Path(path).is_file()]
    if missing_inputs:
        raise ValueError(f"stage {name} is missing inputs: {missing_inputs}")
    expected["inputs"] = {
        str(path): file_sha256(Path(path))
        for path in inputs
    }
    if _stage_is_complete(complete_path, expected, outputs):
        print(f"[stage {name}] resume hit")
        return

    complete_path.unlink(missing_ok=True)
    if clean_outputs_on_rerun:
        for output in outputs:
            output.unlink(missing_ok=True)
            Path(str(output) + "-shm").unlink(missing_ok=True)
            Path(str(output) + "-wal").unlink(missing_ok=True)
    for output in outputs:
        output.parent.mkdir(parents=True, exist_ok=True)
    child_env = dict(os.environ)
    if env_overrides:
        child_env.update(env_overrides)
    print(f"[stage {name}] running: {shlex.join(command)}")
    started_at = datetime.now(timezone.utc).isoformat()
    started = time.monotonic()
    runner(command, cwd=EVALUATION_ROOT, env=child_env, check=True)
    duration_seconds = round(time.monotonic() - started, 3)
    missing = [str(path) for path in outputs if not path.is_file()]
    if missing:
        raise RuntimeError(f"stage {name} did not produce outputs: {missing}")
    completed = dict(expected)
    completed.update(
        {
            "started_at": started_at,
            "finished_at": datetime.now(timezone.utc).isoformat(),
            "duration_seconds": duration_seconds,
        }
    )
    completed["outputs"] = {
        str(path): file_sha256(path)
        for path in outputs
    }
    _write_json_atomic(complete_path, completed)


def _stage_is_complete(
    path: Path,
    expected: dict[str, Any],
    outputs: tuple[Path, ...],
) -> bool:
    if not path.is_file() or not all(output.is_file() for output in outputs):
        return False
    try:
        completed = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return False
    for key, value in expected.items():
        if completed.get(key) != value:
            return False
    hashes = completed.get("outputs") or {}
    return all(hashes.get(str(output)) == file_sha256(output) for output in outputs)


def _write_json_atomic(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n",
        encoding="utf-8",
    )
    temporary.replace(path)

"""Run and atomically record the regression gate for LoCoMo live evaluation."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import subprocess
import sys
import time
from typing import Callable, Sequence

EVALUATION_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(EVALUATION_DIR))

from locomo.locomo_run import (
    EVALUATION_ROOT,
    PROJECT_ROOT,
    REQUIRED_PREFLIGHT_SUITES,
    implementation_hash,
)


REQUIRED_SUITES = REQUIRED_PREFLIGHT_SUITES


def run_preflight(
    output: Path,
    python_executable: str,
    runner: Callable[..., subprocess.CompletedProcess] = subprocess.run,
) -> dict:
    commands = (
        (
            "python_evaluation",
            [python_executable, "-m", "pytest", "-q"],
            EVALUATION_ROOT,
        ),
        ("rust_workspace", ["cargo", "test", "--workspace"], PROJECT_ROOT),
        (
            "shell_syntax",
            [
                "sh",
                "-n",
                str(EVALUATION_ROOT / "run_locomo_eval.sh"),
                str(EVALUATION_ROOT / "run_locomo_memory_ab.sh"),
            ],
            PROJECT_ROOT,
        ),
        ("diff_check", ["git", "diff", "--check"], PROJECT_ROOT),
    )
    started_at = _now()
    suites = []
    for name, command, cwd in commands:
        started = time.monotonic()
        completed = runner(
            command,
            cwd=cwd,
            capture_output=True,
            text=True,
            check=False,
        )
        suites.append(
            {
                "name": name,
                "command": command,
                "cwd": str(cwd),
                "exit_code": int(completed.returncode),
                "duration_seconds": round(time.monotonic() - started, 3),
                "stdout_tail": str(completed.stdout or "")[-4000:],
                "stderr_tail": str(completed.stderr or "")[-4000:],
            }
        )
    report = {
        "schema_version": "locomo-preflight-v1",
        "started_at": started_at,
        "finished_at": _now(),
        "implementation_hash": implementation_hash(),
        "passed": all(item["exit_code"] == 0 for item in suites),
        "suites": suites,
    }
    _write_json_atomic(Path(output), report)
    return report


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _write_json_atomic(path: Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n",
        encoding="utf-8",
    )
    temporary.replace(path)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Run LoCoMo regression preflight.")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)
    report = run_preflight(args.output, python_executable=__import__("sys").executable)
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

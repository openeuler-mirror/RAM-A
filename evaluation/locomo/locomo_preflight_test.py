from __future__ import annotations

import json
from pathlib import Path
import subprocess
import builtins

import pytest

from locomo.locomo_preflight import REQUIRED_SUITES, run_preflight
from locomo.locomo_run import RunConfig, validate_preflight


def test_preflight_records_actual_suite_results_and_hash(tmp_path) -> None:
    calls = []

    def passing_runner(command, **kwargs):
        calls.append((command, kwargs["cwd"]))
        return subprocess.CompletedProcess(
            command,
            0,
            stdout="passed",
            stderr="",
        )

    output = tmp_path / "preflight.json"
    report = run_preflight(output, "/venv/bin/python", runner=passing_runner)

    assert report["passed"] is True
    assert {item["name"] for item in report["suites"]} == set(REQUIRED_SUITES)
    assert all(item["exit_code"] == 0 for item in report["suites"])
    assert len(report["implementation_hash"]) == 64
    assert len(calls) == len(REQUIRED_SUITES)
    assert json.loads(output.read_text(encoding="utf-8")) == report


def test_preflight_failure_is_persisted_but_not_accepted(tmp_path) -> None:
    call_index = 0

    def one_failure(command, **kwargs):
        nonlocal call_index
        call_index += 1
        return subprocess.CompletedProcess(
            command,
            1 if call_index == 2 else 0,
            stdout="",
            stderr="failed" if call_index == 2 else "",
        )

    path = tmp_path / "preflight.json"
    report = run_preflight(path, "/venv/bin/python", runner=one_failure)
    config = RunConfig("raw", "full", tmp_path / "data.json", tmp_path / "run")

    assert report["passed"] is False
    with pytest.raises(ValueError, match="preflight did not pass"):
        validate_preflight(config, path)


def test_preflight_rejects_implementation_hash_tampering(tmp_path) -> None:
    report = {
        "schema_version": "locomo-preflight-v1",
        "passed": True,
        "implementation_hash": "0" * 64,
        "suites": [
            {"name": name, "exit_code": 0}
            for name in REQUIRED_SUITES
        ],
    }
    path = tmp_path / "preflight.json"
    path.write_text(json.dumps(report), encoding="utf-8")
    config = RunConfig("raw", "full", tmp_path / "data.json", tmp_path / "run")

    with pytest.raises(ValueError, match="implementation hash"):
        validate_preflight(config, path)


def test_preflight_validation_does_not_depend_on_package_import_path(
    monkeypatch,
    tmp_path,
) -> None:
    config = RunConfig("raw", "full", tmp_path / "data.json", tmp_path / "run")
    report = {
        "schema_version": "locomo-preflight-v1",
        "passed": True,
        "implementation_hash": config.immutable_manifest()["implementation_hash"],
        "suites": [
            {"name": name, "exit_code": 0}
            for name in REQUIRED_SUITES
        ],
    }
    path = tmp_path / "preflight.json"
    path.write_text(json.dumps(report), encoding="utf-8")
    original_import = builtins.__import__

    def reject_package_import(name, *args, **kwargs):
        if name == "locomo.locomo_preflight":
            raise ModuleNotFoundError("simulated script-mode import path")
        return original_import(name, *args, **kwargs)

    monkeypatch.setattr(builtins, "__import__", reject_package_import)

    assert len(validate_preflight(config, path)) == 64


def test_locomo_accepts_unified_memory_ab_preflight(tmp_path) -> None:
    config = RunConfig("raw", "full", tmp_path / "data.json", tmp_path / "run")
    report = {
        "schema_version": "memory-ab-preflight-v1",
        "dataset": "locomo",
        "passed": True,
        "implementation_hash": config.immutable_manifest()["implementation_hash"],
        "suites": [
            {"name": name, "exit_code": 0}
            for name in (
                "python_evaluation",
                "rust_workspace",
                "rust_clippy",
                "diff_check",
            )
        ],
    }
    path = tmp_path / "preflight.json"
    path.write_text(json.dumps(report), encoding="utf-8")

    assert len(validate_preflight(config, path)) == 64

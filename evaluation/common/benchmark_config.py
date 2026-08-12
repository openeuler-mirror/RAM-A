"""Configuration loading for the user-facing benchmark runner."""

from __future__ import annotations

import os
from pathlib import Path
import tomllib
from typing import Any


DATASETS = {"locomo", "personalmem", "longmemeval"}
MODES = {"normal", "strict"}
PHASES = {"full"}
EXECUTIONS = {"single", "ab"}


def load_benchmark_config(path: Path, dataset: str) -> dict[str, Any]:
    path = Path(path).resolve()
    if dataset not in DATASETS:
        raise ValueError(f"unsupported dataset: {dataset}")
    try:
        raw = tomllib.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        raise ValueError(f"could not read benchmark config: {path}") from error
    except tomllib.TOMLDecodeError as error:
        raise ValueError(f"invalid benchmark config: {path}: {error}") from error

    run = _section(raw, "run")
    phase = str(run.get("phase", "full"))
    mode = str(run.get("mode", "normal"))
    execution = str(run.get("execution", "ab"))
    if phase not in PHASES:
        raise ValueError(f"run.phase must be one of {sorted(PHASES)}")
    if mode not in MODES:
        raise ValueError(f"run.mode must be one of {sorted(MODES)}")
    if execution not in EXECUTIONS:
        raise ValueError(f"run.execution must be one of {sorted(EXECUTIONS)}")
    if execution == "single" and run.get("memory_mode") not in {"raw", "extracted"}:
        raise ValueError("run.memory_mode must be raw or extracted for single runs")

    datasets = raw.get("dataset")
    if not isinstance(datasets, dict) or dataset not in datasets:
        raise ValueError(f"missing dataset.{dataset} configuration")
    dataset_config = datasets[dataset]
    if not isinstance(dataset_config, dict) or not dataset_config.get("file"):
        raise ValueError(f"dataset.{dataset}.file is required")

    dataset_file = _expand_path(str(dataset_config["file"]), path.parent)
    if not dataset_file.is_file():
        raise ValueError(f"dataset file does not exist: {dataset_file}")

    result = dict(raw)
    result["config_path"] = path
    result["dataset_name"] = dataset
    result["dataset_file"] = dataset_file
    result["run"] = {**run, "phase": phase, "mode": mode, "execution": execution}
    result.setdefault("retrieval", {})
    result.setdefault("graph", {})
    result.setdefault("rerank", {})
    result.setdefault("providers", {})
    return result


def _section(raw: dict[str, Any], name: str) -> dict[str, Any]:
    value = raw.get(name, {})
    if not isinstance(value, dict):
        raise ValueError(f"{name} must be a table")
    return value


def _expand_path(value: str, config_dir: Path) -> Path:
    expanded = os.path.expandvars(os.path.expanduser(value))
    path = Path(expanded)
    if not path.is_absolute():
        path = config_dir / path
    return path.resolve()

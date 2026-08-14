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
SECTION_KEYS = {
    "run": {
        "phase",
        "mode",
        "execution",
        "memory_mode",
        "pair_id",
        "output_root",
        "promotion_policy",
        "resume",
    },
    "memory": {
        "extraction_model",
        "verifier_model",
        "api_key_env",
        "base_url",
        "max_candidate_tokens",
        "max_window_tokens",
        "context_before_messages",
        "context_after_messages",
    },
    "embedding": {"provider", "api_key_env", "model", "dimensions"},
    "retrieval": {
        "mode",
        "embedding_weight",
        "bm25_weight",
        "candidate_k",
        "top_k",
    },
    "graph": {
        "enabled",
        "build_enabled",
        "build_concurrency",
        "weight",
        "rerank",
        "allow_graph_only",
        "max_graph_only_results",
        "max_context_facts",
        "fail_open",
        "memory_space_mode",
        "memory_space_field",
        "owner_id",
        "llm_model",
        "llm_api_key_env",
        "llm_base_url",
        "llm_timeout_ms",
    },
    "rerank": {
        "enabled",
        "provider",
        "api_key_env",
        "model",
        "base_url",
        "input_k",
        "timeout_ms",
        "fail_open",
    },
    "answer": {"model", "api_key_env", "base_url", "qa_top_k", "max_tokens"},
    "judge": {"model", "api_key_env", "base_url"},
}
TOP_LEVEL_KEYS = {*SECTION_KEYS, "dataset"}
BOOLEAN_FIELDS = {
    "run": {"resume"},
    "graph": {
        "enabled",
        "build_enabled",
        "rerank",
        "allow_graph_only",
        "fail_open",
    },
    "rerank": {"enabled", "fail_open"},
}


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

    _validate_schema(raw)
    _validate_boolean_fields(raw)

    run = _section(raw, "run")
    phase = str(run.get("phase", "full"))
    mode = str(run.get("mode", "normal"))
    execution = str(run.get("execution", "ab"))
    resume = run.get("resume", False)
    if phase not in PHASES:
        raise ValueError(f"run.phase must be one of {sorted(PHASES)}")
    if mode not in MODES:
        raise ValueError(f"run.mode must be one of {sorted(MODES)}")
    if execution not in EXECUTIONS:
        raise ValueError(f"run.execution must be one of {sorted(EXECUTIONS)}")
    if execution == "single" and run.get("memory_mode") not in {"raw", "extracted"}:
        raise ValueError("run.memory_mode must be raw or extracted for single runs")
    if execution == "single" and mode != "normal":
        raise ValueError("single runs only support normal mode")
    if execution == "ab" and run.get("memory_mode") is not None:
        raise ValueError("run.memory_mode is only valid for single runs")

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
    result["run"] = {
        **run,
        "phase": phase,
        "mode": mode,
        "execution": execution,
        "resume": resume,
    }
    result.setdefault("retrieval", {})
    result.setdefault("graph", {})
    result.setdefault("rerank", {})
    if dataset == "longmemeval":
        answer = _section(result, "answer")
        judge = _section(result, "judge")
        answer_key = str(answer.get("api_key_env", "OPENROUTER_API_KEY"))
        judge_key = str(judge.get("api_key_env", answer_key))
        answer_url = str(
            answer.get("base_url", "https://openrouter.ai/api/v1")
        )
        judge_url = str(judge.get("base_url", answer_url))
        if (answer_key, answer_url) != (judge_key, judge_url):
            raise ValueError(
                "LongMemEval answer and judge must use the same api_key_env "
                "and base_url"
            )
    return result


def _validate_schema(raw: dict[str, Any]) -> None:
    unknown_sections = sorted(set(raw) - TOP_LEVEL_KEYS)
    if unknown_sections:
        raise ValueError(
            f"unknown benchmark config section: {unknown_sections[0]}"
        )
    for section_name, allowed_keys in SECTION_KEYS.items():
        section = raw.get(section_name)
        if section is None:
            continue
        if not isinstance(section, dict):
            raise ValueError(f"{section_name} must be a table")
        unknown_keys = sorted(set(section) - allowed_keys)
        if unknown_keys:
            raise ValueError(
                f"unknown {section_name} config key: {unknown_keys[0]}"
            )
    datasets = raw.get("dataset")
    if datasets is None:
        return
    if not isinstance(datasets, dict):
        raise ValueError("dataset must be a table")
    unknown_datasets = sorted(set(datasets) - DATASETS)
    if unknown_datasets:
        raise ValueError(f"unknown dataset config: {unknown_datasets[0]}")
    for dataset_name, dataset_config in datasets.items():
        if not isinstance(dataset_config, dict):
            raise ValueError(f"dataset.{dataset_name} must be a table")
        unknown_keys = sorted(set(dataset_config) - {"file"})
        if unknown_keys:
            raise ValueError(
                f"unknown dataset.{dataset_name} config key: {unknown_keys[0]}"
            )


def _validate_boolean_fields(raw: dict[str, Any]) -> None:
    for section_name, field_names in BOOLEAN_FIELDS.items():
        section = raw.get(section_name, {})
        if not isinstance(section, dict):
            continue
        for field_name in field_names:
            value = section.get(field_name)
            if value is not None and not isinstance(value, bool):
                raise ValueError(
                    f"{section_name}.{field_name} must be a boolean"
                )


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

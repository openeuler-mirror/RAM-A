"""Shared promotion checks and compact history records for memory A/B runs."""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from numbers import Real
from pathlib import Path
from typing import Any


POLICY_SCHEMA_VERSION = "memory-ab-promotion-v1"
HISTORY_SCHEMA_VERSION = "memory-ab-history-v1"
HISTORY_CONFIGURATION_KEYS = (
    "backend",
    "store_backend",
    "embedding",
    "embedding_provider",
    "embedding_model",
    "embedding_dimensions",
    "embedding_batch_size",
    "api_key_env",
    "credential_env",
    "search_mode",
    "embedding_weight",
    "bm25_weight",
    "candidate_k",
    "top_k",
    "retrieval_top_k",
    "qa_top_k",
    "graph",
    "graph_enabled",
    "graph_build",
    "graph_build_enabled",
    "graph_build_concurrency",
    "graph_weight",
    "graph_rerank",
    "graph_allow_graph_only",
    "graph_max_graph_only_results",
    "graph_fail_open",
    "graph_memory_space_mode",
    "graph_memory_space_field",
    "graph_owner_id",
    "graph_llm_api_key_env",
    "graph_llm_model",
    "graph_llm_base_url",
    "graph_llm_timeout_ms",
    "max_graph_context_facts",
    "rerank",
    "rerank_enabled",
    "rerank_provider",
    "rerank_api_key_env",
    "rerank_model",
    "rerank_base_url",
    "rerank_input_k",
    "rerank_timeout_ms",
    "rerank_fail_open",
    "answer_model",
    "answerer_model",
    "chat_model",
    "answer_api_key_env",
    "answer_base_url",
    "judge_model",
    "judge_api_key_env",
    "judge_base_url",
    "llm_api_key_env",
    "llm_base_url",
    "llm_thinking",
    "show_scores",
    "answer_prompt_version",
    "memory_format",
    "answer_max_tokens",
    "context_token_budget",
    "max_retries",
    "retry_backoff_seconds",
    "text_fields",
    "query_fields",
    "gold_fields",
    "max_candidate_tokens",
    "max_window_tokens",
    "context_before_messages",
    "context_after_messages",
    "extraction_model",
    "verifier_model",
    "extraction_api_key_env",
    "extraction_base_url",
    "pipeline_phase",
    "prompt_versions",
    "extraction_schema_version",
    "llm_temperature",
)


@dataclass(frozen=True)
class PromotionPolicy:
    primary_metric: str
    historical_floor: float
    require_strict_raw_improvement: bool = True
    category_floors: dict[str, float] = field(default_factory=dict)
    completeness_counts: dict[str, int] = field(default_factory=dict)
    cost_ceilings: dict[str, float] = field(default_factory=dict)
    schema_version: str = POLICY_SCHEMA_VERSION

    def __post_init__(self) -> None:
        if self.schema_version != POLICY_SCHEMA_VERSION:
            raise ValueError(
                f"unsupported promotion policy schema: {self.schema_version}"
            )
        if not isinstance(self.require_strict_raw_improvement, bool):
            raise ValueError("require_strict_raw_improvement must be boolean")
        _validate_metric_path(self.primary_metric)
        _validate_policy_number("historical_floor", self.historical_floor)
        for collection_name, values in (
            ("category_floors", self.category_floors),
            ("completeness_counts", self.completeness_counts),
            ("cost_ceilings", self.cost_ceilings),
        ):
            if not isinstance(values, dict):
                raise ValueError(f"{collection_name} must be an object")
            for path, value in values.items():
                _validate_metric_path(path)
                _validate_policy_number(f"{collection_name}.{path}", value)
        if any(
            isinstance(value, bool) or not isinstance(value, int) or value < 0
            for value in self.completeness_counts.values()
        ):
            raise ValueError("completeness counts must be non-negative integers")

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "PromotionPolicy":
        if not isinstance(value, dict):
            raise ValueError("promotion policy must be a JSON object")
        if "schema_version" not in value:
            raise ValueError("promotion policy requires schema_version")
        return cls(
            primary_metric=value["primary_metric"],
            historical_floor=value["historical_floor"],
            require_strict_raw_improvement=value.get(
                "require_strict_raw_improvement", True
            ),
            category_floors=dict(value.get("category_floors", {})),
            completeness_counts=dict(value.get("completeness_counts", {})),
            cost_ceilings=dict(value.get("cost_ceilings", {})),
            schema_version=value["schema_version"],
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "schema_version": self.schema_version,
            "primary_metric": self.primary_metric,
            "historical_floor": self.historical_floor,
            "require_strict_raw_improvement": self.require_strict_raw_improvement,
            "category_floors": dict(self.category_floors),
            "completeness_counts": dict(self.completeness_counts),
            "cost_ceilings": dict(self.cost_ceilings),
        }


def evaluate_checks(
    policy: PromotionPolicy,
    raw_metrics: dict[str, Any],
    extracted_metrics: dict[str, Any],
) -> dict[str, Any]:
    raw_primary = _numeric_metric(raw_metrics, policy.primary_metric)
    extracted_primary = _numeric_metric(extracted_metrics, policy.primary_metric)
    checks = [
        _check(
            "historical_primary",
            extracted_primary >= policy.historical_floor,
            extracted_primary,
            ">=",
            policy.historical_floor,
        )
    ]
    if policy.require_strict_raw_improvement:
        checks.append(
            _check(
                "fresh_raw_primary",
                extracted_primary > raw_primary,
                extracted_primary,
                ">",
                raw_primary,
            )
        )
    for path, floor in sorted(policy.category_floors.items()):
        actual = _numeric_metric(extracted_metrics, path)
        checks.append(
            _check(f"category_floor:{path}", actual >= floor, actual, ">=", floor)
        )
    for path, expected in sorted(policy.completeness_counts.items()):
        raw_count = _numeric_metric(raw_metrics, path)
        extracted_count = _numeric_metric(extracted_metrics, path)
        checks.extend(
            (
                _check(
                    f"raw_completeness:{path}",
                    raw_count == expected,
                    raw_count,
                    "==",
                    expected,
                ),
                _check(
                    f"extracted_completeness:{path}",
                    extracted_count == expected,
                    extracted_count,
                    "==",
                    expected,
                ),
            )
        )
    for path, ceiling in sorted(policy.cost_ceilings.items()):
        actual = _numeric_metric(extracted_metrics, path)
        checks.append(
            _check(
                f"cost_ceiling:{path}",
                actual <= ceiling,
                actual,
                "<=",
                ceiling,
            )
        )
    reasons = [check["name"] for check in checks if not check["passed"]]
    return {"passed": not reasons, "checks": checks, "reasons": reasons}


def build_history_record(
    comparison: dict[str, Any], memory_mode: str
) -> dict[str, Any]:
    if comparison.get("phase") != "full" or comparison.get("complete") is not True:
        raise ValueError("history records require a completed full pair")
    if memory_mode not in {"raw", "extracted"}:
        raise ValueError(f"unsupported history memory mode: {memory_mode}")

    contract = comparison["arm_contract"]
    promotion = comparison["promotion"]
    governance_mode = comparison.get("governance_mode", "strict")
    if governance_mode not in {"normal", "strict"}:
        raise ValueError("history record governance_mode must be normal or strict")
    if governance_mode == "strict" and not isinstance(promotion.get("passed"), bool):
        raise ValueError("history record promotion passed must be boolean")
    reasons = promotion.get("reasons")
    if (
        not isinstance(reasons, list)
        or any(not isinstance(reason, str) or not reason for reason in reasons)
        or (
            governance_mode == "strict"
            and promotion.get("passed") is False
            and not reasons
        )
    ):
        raise ValueError("history record promotion reasons are invalid")
    arm = comparison["fresh_raw" if memory_mode == "raw" else "treatment"]
    artifact_path = arm.get("artifact_path")
    if artifact_path is None or (isinstance(artifact_path, str) and not artifact_path):
        raise ValueError("history record requires artifact_path")
    metrics = _portable_history_value(arm["metrics"])
    record = {
        "schema_version": HISTORY_SCHEMA_VERSION,
        "pair_id": comparison["pair_id"],
        "run_id": arm["run_id"],
        "dataset": comparison["dataset"],
        "split": comparison["split"],
        "memory_mode": memory_mode,
        "phase": comparison["phase"],
        "source_hash": contract["source_hash"],
        "code_hash": contract["implementation_hash"],
        "configuration_hash": contract["configuration_hash"],
        "preflight_hash": contract.get("preflight_hash"),
        "policy_hash": comparison.get("policy_hash"),
        "governance_mode": governance_mode,
        "configuration": arm["configuration"],
        "metrics": metrics,
        "promotion_status": (
            "not_evaluated"
            if governance_mode == "normal"
            else (
                "reference"
                if memory_mode == "raw"
                else ("passed" if promotion["passed"] else "failed")
            )
        ),
        "promotion_reasons": (
            []
            if governance_mode == "normal" or memory_mode == "raw"
            else list(reasons)
        ),
        "artifact_path": _portable_history_path(artifact_path),
    }
    for key, value in record.items():
        nullable = governance_mode == "normal" and key in {"preflight_hash", "policy_hash"}
        if not nullable and (value is None or (isinstance(value, str) and not value)):
            raise ValueError(f"history record requires {key}")
    if not isinstance(record["configuration"], dict):
        raise ValueError("history record configuration must be an object")
    if not isinstance(record["metrics"], dict):
        raise ValueError("history record metrics must be an object")
    return record


def history_configuration(config: dict[str, Any]) -> dict[str, Any]:
    """Select behavior-affecting, non-secret settings for history records."""
    return {
        key: config[key]
        for key in HISTORY_CONFIGURATION_KEYS
        if key in config and config[key] is not None
    }


def _portable_history_path(value: Any) -> str:
    """Keep repository-local artifacts portable across machines."""
    value = str(value)
    if "://" in value:
        return value
    path = Path(value)
    if not path.is_absolute():
        return path.as_posix()
    parts = path.parts
    try:
        outputs_index = max(
            index for index, component in enumerate(parts) if component == "outputs"
        )
    except ValueError:
        return path.as_posix()
    return Path(*parts[outputs_index:]).as_posix()


def _portable_history_value(
    value: Any, path: tuple[str, ...] = ()
) -> Any:
    """Normalize repository-local paths embedded in history metrics."""
    if isinstance(value, dict):
        return {
            child_key: _portable_history_value(child, (*path, child_key))
            for child_key, child in value.items()
        }
    if isinstance(value, list):
        return [_portable_history_value(child, path) for child in value]
    if path[-2:] == ("retrieval", "input") and isinstance(value, str):
        return _portable_history_path(value)
    return value


def build_history_records(comparison: dict[str, Any]) -> list[dict[str, Any]]:
    return [
        build_history_record(comparison, "raw"),
        build_history_record(comparison, "extracted"),
    ]


def resolve_history_artifact_path(
    comparison_path: Path, requested_path: Path | None = None
) -> Path:
    comparison_path = Path(comparison_path)
    history_path = Path(requested_path or comparison_path.parent / "history_record.json")
    if comparison_path.resolve() == history_path.resolve():
        raise ValueError("comparison and history artifact paths must be distinct")
    return history_path


def remove_stale_history_artifact(path: Path) -> None:
    Path(path).unlink(missing_ok=True)


def _numeric_metric(metrics: dict[str, Any], path: str) -> float | int:
    value: Any = metrics
    for segment in path.split("."):
        if not segment or not isinstance(value, dict) or segment not in value:
            raise ValueError(f"missing metric path: {path}")
        value = value[segment]
    if isinstance(value, bool) or not isinstance(value, Real):
        raise ValueError(f"non-numeric metric path: {path}")
    if not math.isfinite(float(value)):
        raise ValueError(f"non-numeric metric path: {path}")
    return value


def _validate_metric_path(path: Any) -> None:
    if (
        not isinstance(path, str)
        or "." not in path
        or any(not segment for segment in path.split("."))
    ):
        raise ValueError(f"metric path must be explicit dotted path: {path!r}")


def _validate_policy_number(name: str, value: Any) -> None:
    if (
        isinstance(value, bool)
        or not isinstance(value, Real)
        or not math.isfinite(float(value))
    ):
        raise ValueError(f"{name} must be numeric")


def _check(
    name: str,
    passed: bool,
    actual: float | int,
    operator: str,
    threshold: float | int,
) -> dict[str, Any]:
    return {
        "name": name,
        "passed": bool(passed),
        "actual": actual,
        "operator": operator,
        "threshold": threshold,
    }

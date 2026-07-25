"""Shared promotion checks and compact history records for memory A/B runs."""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from numbers import Real
from pathlib import Path
from typing import Any


POLICY_SCHEMA_VERSION = "memory-ab-promotion-v1"
HISTORY_SCHEMA_VERSION = "memory-ab-history-v1"


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
    if not isinstance(promotion.get("passed"), bool):
        raise ValueError("history record promotion passed must be boolean")
    reasons = promotion.get("reasons")
    if (
        not isinstance(reasons, list)
        or any(not isinstance(reason, str) or not reason for reason in reasons)
        or (promotion["passed"] is False and not reasons)
    ):
        raise ValueError("history record promotion reasons are invalid")
    arm = comparison["fresh_raw" if memory_mode == "raw" else "treatment"]
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
        "preflight_hash": contract["preflight_hash"],
        "policy_hash": comparison["policy_hash"],
        "configuration": arm["configuration"],
        "metrics": arm["metrics"],
        "promotion_status": (
            "reference"
            if memory_mode == "raw"
            else ("passed" if promotion["passed"] else "failed")
        ),
        "promotion_reasons": (
            [] if memory_mode == "raw" else list(reasons)
        ),
        "artifact_path": arm["artifact_path"],
    }
    for key, value in record.items():
        if value is None or (isinstance(value, str) and not value):
            raise ValueError(f"history record requires {key}")
    if not isinstance(record["configuration"], dict):
        raise ValueError("history record configuration must be an object")
    if not isinstance(record["metrics"], dict):
        raise ValueError("history record metrics must be an object")
    return record


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

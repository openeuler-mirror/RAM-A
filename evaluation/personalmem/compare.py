"""Build governed PersonaMem raw-vs-extracted comparisons."""

from __future__ import annotations

import argparse
import json
from numbers import Real
from pathlib import Path
from typing import Any, Sequence

from common.memory_ab import file_sha256, validate_pair_contract
from common.memory_ab_compare import (
    PromotionPolicy,
    build_history_records,
    evaluate_checks,
    remove_stale_history_artifact,
    resolve_history_artifact_path,
)


def build_comparison(
    raw_config: dict[str, Any],
    extracted_config: dict[str, Any],
    raw_prepared: dict[str, Any],
    extracted_prepared: dict[str, Any],
    raw_grades: dict[str, Any],
    extracted_grades: dict[str, Any],
    policy: PromotionPolicy,
) -> dict[str, Any]:
    contract = validate_pair_contract(
        raw_config,
        extracted_config,
        raw_prepared,
        extracted_prepared,
    )
    phase = _matching_required(raw_config, extracted_config, "phase")
    pair_id = _matching_required(raw_config, extracted_config, "pair_id")
    policy_hash = _matching_required(
        raw_config, extracted_config, "promotion_policy_hash"
    )
    _require_dataset(raw_config)
    _require_dataset(extracted_config)
    split = _prepared_split(raw_prepared)

    raw_metrics = _grade_metrics(raw_grades)
    extracted_metrics = _grade_metrics(extracted_grades)
    promotion = evaluate_checks(policy, raw_metrics, extracted_metrics)
    expected_count = contract["query_count"]
    complete = all(
        metrics["qa"]["overall"]["count"] == expected_count
        for metrics in (raw_metrics, extracted_metrics)
    )
    if phase == "full":
        complete = complete and _completeness_checks_passed(policy, promotion)
    if not complete:
        promotion = _fail_incomplete(promotion, expected_count)

    report = {
        "schema_version": "memory-ab-comparison-v1",
        "dataset": "personalmem",
        "split": split,
        "phase": phase,
        "pair_id": pair_id,
        "complete": complete,
        "arm_contract": {
            **contract,
            "source_hash": raw_config["source_hash"],
            "implementation_hash": raw_config["implementation_hash"],
            "preflight_hash": raw_config["preflight_hash"],
        },
        "policy_hash": policy_hash,
        "policy": policy.to_dict(),
        "fresh_raw": _arm(raw_config, raw_metrics),
        "treatment": _arm(extracted_config, extracted_metrics),
        "promotion": promotion,
    }
    if phase == "full" and complete:
        report["history_records"] = build_history_records(report)
    return report


def _grade_metrics(grades: dict[str, Any]) -> dict[str, Any]:
    summary = grades.get("summary")
    groups = grades.get("by_question_type")
    if not isinstance(summary, dict) or not isinstance(groups, list):
        raise ValueError("PersonaMem grade metrics are incomplete")
    overall = {
        "accuracy": _number(summary, "answer_acc"),
        "count": _integer(summary, "total"),
        "correct": _integer(summary, "correct"),
    }
    by_question_type: dict[str, dict[str, Any]] = {}
    for group in groups:
        if not isinstance(group, dict) or not str(group.get("name") or ""):
            raise ValueError("PersonaMem grade question type is missing")
        name = str(group["name"])
        if name in by_question_type:
            raise ValueError(f"duplicate PersonaMem grade question type: {name}")
        by_question_type[name] = {
            "accuracy": _number(group, "accuracy"),
            "count": _integer(group, "total"),
            "correct": _integer(group, "correct"),
        }
    metrics: dict[str, Any] = {
        "qa": {
            "overall": overall,
            "by_question_type": by_question_type,
        }
    }
    if isinstance(grades.get("cost"), dict):
        metrics["cost"] = dict(grades["cost"])
    return metrics


def _arm(config: dict[str, Any], metrics: dict[str, Any]) -> dict[str, Any]:
    run_id = config.get("run_id")
    run_dir = config.get("run_dir")
    if not run_id and run_dir:
        run_id = Path(str(run_dir)).name
    artifact_path = config.get("artifact_path") or run_dir
    if not run_id:
        raise ValueError("arm config requires run_id or run_dir")
    if not artifact_path:
        raise ValueError("arm config requires artifact_path or run_dir")
    compact_keys = (
        "backend",
        "top_k",
        "candidate_k",
        "search_mode",
        "embedding_model",
        "embedding_dimensions",
        "answer_model",
        "context_token_budget",
    )
    return {
        "run_id": str(run_id),
        "configuration": {
            key: config[key]
            for key in compact_keys
            if key in config and config[key] is not None
        },
        "metrics": metrics,
        "artifact_path": str(artifact_path),
    }


def _matching_required(
    raw: dict[str, Any], extracted: dict[str, Any], key: str
) -> Any:
    value = raw.get(key)
    if value is None or value == "" or value != extracted.get(key):
        raise ValueError(f"raw/extracted {key} mismatch")
    return value


def _require_dataset(config: dict[str, Any]) -> None:
    if config.get("dataset") != "personalmem":
        raise ValueError("PersonaMem config dataset mismatch")


def _prepared_split(prepared: dict[str, Any]) -> str:
    split = (prepared.get("dataset") or {}).get("split")
    if not split:
        raise ValueError("PersonaMem prepared split is missing")
    return str(split)


def _number(value: dict[str, Any], key: str) -> float | int:
    result = value.get(key)
    if isinstance(result, bool) or not isinstance(result, Real):
        raise ValueError(f"PersonaMem grade {key} is missing or non-numeric")
    return result


def _integer(value: dict[str, Any], key: str) -> int:
    result = _number(value, key)
    if not isinstance(result, int) or result < 0:
        raise ValueError(f"PersonaMem grade {key} must be a non-negative integer")
    return result


def _fail_incomplete(promotion: dict[str, Any], expected_count: int) -> dict[str, Any]:
    checks = list(promotion["checks"])
    if "incomplete_pair" not in promotion["reasons"]:
        checks.append(
            {
                "name": "incomplete_pair",
                "passed": False,
                "actual": "incomplete",
                "operator": "==",
                "threshold": expected_count,
            }
        )
    reasons = [check["name"] for check in checks if not check["passed"]]
    return {"passed": False, "checks": checks, "reasons": reasons}


def _completeness_checks_passed(
    policy: PromotionPolicy, promotion: dict[str, Any]
) -> bool:
    checks = [
        check
        for check in promotion["checks"]
        if check["name"].startswith(("raw_completeness:", "extracted_completeness:"))
    ]
    return (
        bool(policy.completeness_counts)
        and len(checks) == 2 * len(policy.completeness_counts)
        and all(check["passed"] for check in checks)
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Compare paired PersonaMem raw and extracted runs."
    )
    parser.add_argument("--raw-dir", required=True, type=Path)
    parser.add_argument("--treatment-dir", required=True, type=Path)
    parser.add_argument("--policy", required=True, type=Path)
    parser.add_argument("--output-json", required=True, type=Path)
    parser.add_argument("--history-record", type=Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    history_path = resolve_history_artifact_path(
        args.output_json, args.history_record
    )
    raw_config = _read_object(args.raw_dir / "config.json")
    extracted_config = _read_object(args.treatment_dir / "config.json")
    policy_value = _read_object(args.policy)
    policy_hash = file_sha256(args.policy)
    if any(
        config.get("promotion_policy_hash") != policy_hash
        for config in (raw_config, extracted_config)
    ):
        raise ValueError("promotion policy hash does not match paired configs")
    report = build_comparison(
        raw_config,
        extracted_config,
        _read_object(args.raw_dir / "raw_prepared.json"),
        _read_object(args.treatment_dir / "raw_prepared.json"),
        _read_object(args.raw_dir / "grade_metrics.json"),
        _read_object(args.treatment_dir / "grade_metrics.json"),
        PromotionPolicy.from_dict(policy_value),
    )
    _write_json_atomic(args.output_json, report)
    if "history_records" in report:
        _write_json_atomic(history_path, report["history_records"])
    else:
        remove_stale_history_artifact(history_path)
    return 0


def _read_object(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def _write_json_atomic(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n",
        encoding="utf-8",
    )
    temporary.replace(path)


if __name__ == "__main__":
    raise SystemExit(main())

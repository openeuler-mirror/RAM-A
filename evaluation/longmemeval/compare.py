"""Build governed LongMemEval raw-vs-extracted comparisons."""

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
    raw_qa: dict[str, Any],
    extracted_qa: dict[str, Any],
    raw_retrieval: dict[str, Any],
    extracted_retrieval: dict[str, Any],
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

    raw_metrics = _metrics(raw_qa, raw_retrieval)
    extracted_metrics = _metrics(extracted_qa, extracted_retrieval)
    promotion = evaluate_checks(policy, raw_metrics, extracted_metrics)
    query_count = contract["query_count"]
    retrieval_count = sum(
        1
        for query in raw_prepared["queries"]
        if not str(query["id"]).endswith("_abs")
    )
    complete = all(
        metrics["qa"]["overall"]["count"] == query_count
        and metrics["retrieval"]["overall"]["count"] == retrieval_count
        and metrics["retrieval"]["overall"]["missing"] == 0
        for metrics in (raw_metrics, extracted_metrics)
    )
    if phase == "full":
        complete = complete and _completeness_checks_passed(policy, promotion)
    if not complete:
        promotion = _fail_incomplete(promotion, query_count, retrieval_count)

    report = {
        "schema_version": "memory-ab-comparison-v1",
        "dataset": "longmemeval",
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


def _metrics(qa: dict[str, Any], retrieval: dict[str, Any]) -> dict[str, Any]:
    qa_overall = _qa_group(qa.get("overall"), "overall")
    qa_by_type = _group_map(qa.get("by_type"), _qa_group, "QA")
    session = retrieval.get("session")
    turn = retrieval.get("turn")
    if not isinstance(session, dict) or not isinstance(turn, dict):
        raise ValueError("LongMemEval retrieval metrics are incomplete")
    metrics: dict[str, Any] = {
        "qa": {"overall": qa_overall, "by_type": qa_by_type},
        "retrieval": {
            "overall": {
                "count": _integer(retrieval, "num_evaluated", "retrieval"),
                "missing": _integer(
                    retrieval, "num_missing_results", "retrieval"
                ),
            },
            "session": {
                "overall": _session_group(session.get("overall"), "overall"),
                "by_type": _group_map(
                    session.get("by_type"), _session_group, "session retrieval"
                ),
            },
            "turn": {
                "overall": _turn_group(turn.get("overall"), "overall"),
                "by_type": _group_map(
                    turn.get("by_type"), _turn_group, "turn retrieval"
                ),
            },
        },
    }
    if isinstance(qa.get("cost"), dict):
        metrics["cost"] = dict(qa["cost"])
    return metrics


def _qa_group(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError(f"LongMemEval QA {label} metrics are missing")
    return {
        "accuracy": _number(value, "accuracy", f"QA {label}"),
        "count": _integer(value, "total", f"QA {label}"),
        "correct": _integer(value, "correct", f"QA {label}"),
    }


def _session_group(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError(f"LongMemEval session {label} metrics are missing")
    return {
        "recall_at_10": _number(value, "recall@10", f"session {label}"),
    }


def _turn_group(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError(f"LongMemEval turn {label} metrics are missing")
    return {
        "recall_at_10": _number(value, "recall@10", f"turn {label}"),
        "mrr": _number(value, "mrr", f"turn {label}"),
    }


def _group_map(value: Any, parser: Any, label: str) -> dict[str, dict[str, Any]]:
    if not isinstance(value, dict):
        raise ValueError(f"LongMemEval {label} by-type metrics are missing")
    return {
        str(name): parser(group, str(name))
        for name, group in sorted(value.items())
    }


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
        "embedding_model",
        "embedding_dimensions",
        "retrieval_top_k",
        "qa_top_k",
        "answerer_model",
        "judge_model",
        "answer_prompt_version",
        "memory_format",
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
    if config.get("dataset") != "longmemeval":
        raise ValueError("LongMemEval config dataset mismatch")


def _prepared_split(prepared: dict[str, Any]) -> str:
    split = (prepared.get("dataset") or {}).get("split")
    if not split:
        raise ValueError("LongMemEval prepared split is missing")
    return str(split)


def _number(value: dict[str, Any], key: str, label: str) -> float | int:
    result = value.get(key)
    if isinstance(result, bool) or not isinstance(result, Real):
        raise ValueError(f"LongMemEval {label} {key} is missing or non-numeric")
    return result


def _integer(value: dict[str, Any], key: str, label: str) -> int:
    result = _number(value, key, label)
    if not isinstance(result, int) or result < 0:
        raise ValueError(
            f"LongMemEval {label} {key} must be a non-negative integer"
        )
    return result


def _fail_incomplete(
    promotion: dict[str, Any], qa_count: int, retrieval_count: int
) -> dict[str, Any]:
    checks = list(promotion["checks"])
    checks.append(
        {
            "name": "incomplete_pair",
            "passed": False,
            "actual": "incomplete",
            "operator": "==",
            "threshold": {
                "qa_count": qa_count,
                "retrieval_count": retrieval_count,
            },
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
        description="Compare paired LongMemEval raw and extracted runs."
    )
    parser.add_argument("--raw-dir", required=True, type=Path)
    parser.add_argument("--treatment-dir", required=True, type=Path)
    parser.add_argument("--policy", required=True, type=Path)
    parser.add_argument("--output-json", required=True, type=Path)
    parser.add_argument("--history-record", type=Path)
    parser.add_argument("--raw-qa-metrics", type=Path)
    parser.add_argument("--treatment-qa-metrics", type=Path)
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
        _read_object(args.raw_qa_metrics or _qa_metrics_path(args.raw_dir)),
        _read_object(
            args.treatment_qa_metrics or _qa_metrics_path(args.treatment_dir)
        ),
        _read_object(args.raw_dir / "metrics.json"),
        _read_object(args.treatment_dir / "metrics.json"),
        PromotionPolicy.from_dict(policy_value),
    )
    _write_json_atomic(args.output_json, report)
    if "history_records" in report:
        _write_json_atomic(history_path, report["history_records"])
    else:
        remove_stale_history_artifact(history_path)
    return 0


def _qa_metrics_path(run_dir: Path) -> Path:
    simple = run_dir / "qa_metrics.json"
    if simple.is_file():
        return simple
    matches = sorted(run_dir.glob("qa_metrics_*.json"))
    if len(matches) != 1:
        raise ValueError(
            f"{run_dir} must contain exactly one qa_metrics_*.json artifact"
        )
    return matches[0]


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

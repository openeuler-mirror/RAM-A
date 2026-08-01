"""Build paired LoCoMo raw-vs-extracted comparison and promotion reports."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import sys
from typing import Any, Sequence

EVALUATION_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(EVALUATION_ROOT))

from common.memory_ab import canonical_sha256, file_sha256
from common.memory_ab_compare import (
    build_history_records,
    remove_stale_history_artifact,
    resolve_history_artifact_path,
)
from common.report import generate_report, html_escape
from locomo.locomo_metric import aggregate_scores


HISTORICAL_V3 = {
    "label": "v3 +rerank cohere-v3.5 2026-07-08",
    "overall": {"llm_score": 0.4065, "count": 1540},
    "by_category": {
        "1": {"llm_score": 0.2199},
        "2": {"llm_score": 0.4361},
        "3": {"llm_score": 0.2917},
        "4": {"llm_score": 0.4709},
    },
    "retrieval": {"evidence_hit_at_k": 0.3230, "evidence_mrr": 0.1410},
}
FULL_THRESHOLDS = {
    "1": 0.1999,
    "2": 0.4161,
    "3": 0.2717,
    "4": 0.4509,
}


def promotion_policy_manifest() -> dict[str, Any]:
    return {
        "schema_version": "locomo-promotion-v1",
        "historical_overall": {"operator": ">", "threshold": 0.4065},
        "fresh_raw_overall": {"operator": ">"},
        "scored_count": 1540,
        "category_floors": dict(FULL_THRESHOLDS),
        "regression_suite_required": True,
    }


def validate_paired_query_ids(raw: dict, treatment: dict) -> None:
    raw_ids = _ordered_query_ids(raw)
    treatment_ids = _ordered_query_ids(treatment)
    if len(raw_ids) != len(set(raw_ids)) or len(treatment_ids) != len(
        set(treatment_ids)
    ):
        raise ValueError("paired query ids contain duplicates")
    if set(raw_ids) != set(treatment_ids):
        raise ValueError("paired query ids differ between raw and treatment")
    if raw_ids != treatment_ids:
        raise ValueError("paired query order differs between raw and treatment")


def validate_arm_contract(
    raw_config: dict,
    treatment_config: dict,
    raw_prepared: dict,
    treatment_prepared: dict,
    raw_judged: dict,
    treatment_judged: dict,
) -> dict[str, Any]:
    if raw_config.get("memory_mode") != "raw":
        raise ValueError("fresh raw config does not declare memory_mode raw")
    if treatment_config.get("memory_mode") != "extracted":
        raise ValueError("treatment config does not declare memory_mode extracted")
    for key, label in (
        ("source_hash", "source hash"),
        ("configuration_hash", "configuration hash"),
        ("implementation_hash", "implementation hash"),
        ("preflight_hash", "preflight hash"),
    ):
        if not raw_config.get(key) or raw_config.get(key) != treatment_config.get(key):
            raise ValueError(f"raw/treatment {label} mismatch")
    raw_queries = raw_prepared.get("queries")
    treatment_queries = treatment_prepared.get("queries")
    if raw_queries != treatment_queries or not isinstance(raw_queries, list):
        raise ValueError("raw/treatment prepared queries differ")
    authoritative_ids = [
        str(query.get("id") or "")
        for query in raw_queries
        if int((query.get("task") or {}).get("category", -1)) != 5
    ]
    if not all(authoritative_ids) or len(authoritative_ids) != len(set(authoritative_ids)):
        raise ValueError("authoritative scored query ids are missing or duplicated")
    raw_ids = _ordered_query_ids(raw_judged)
    treatment_ids = _ordered_query_ids(treatment_judged)
    if raw_ids != authoritative_ids or treatment_ids != authoritative_ids:
        raise ValueError("judge results do not match authoritative scored query ids")
    return {
        "source_hash": raw_config["source_hash"],
        "configuration_hash": raw_config["configuration_hash"],
        "implementation_hash": raw_config["implementation_hash"],
        "preflight_hash": raw_config["preflight_hash"],
        "query_count": len(raw_queries),
        "scored_query_count": len(authoritative_ids),
        "raw_prepared_hash": _object_hash(raw_prepared),
        "treatment_prepared_hash": _object_hash(treatment_prepared),
    }


def build_comparison(
    phase: str,
    raw_judged: dict,
    treatment_judged: dict,
    raw_retrieval: dict,
    treatment_retrieval: dict,
    pipeline_stats: dict,
    config: dict,
) -> dict[str, Any]:
    if phase not in {"pilot", "full"}:
        raise ValueError(f"unsupported comparison phase: {phase}")
    validate_paired_query_ids(raw_judged, treatment_judged)

    raw = _arm_summary(raw_judged)
    treatment = _arm_summary(treatment_judged)
    report: dict[str, Any] = {
        "schema_version": "locomo-memory-ab-v1",
        "phase": phase,
        "historical": HISTORICAL_V3,
        "fresh_raw": raw,
        "treatment": treatment,
        "delta": _score_delta(raw, treatment),
        "retrieval": {
            "historical": HISTORICAL_V3["retrieval"],
            "fresh_raw": raw_retrieval,
            "treatment": treatment_retrieval,
        },
        "pipeline": pipeline_stats,
        "configuration": config,
        "verification": {
            "regression_passed": bool(config.get("regression_passed", False))
        },
        "cost": _cost_summary(raw_judged, treatment_judged, pipeline_stats, config),
    }
    checks = pilot_checks(report) if phase == "pilot" else promotion_checks(report)
    report["promotion"] = {
        "passed": all(item["passed"] for item in checks),
        "checks": checks,
    }
    return report


def promotion_checks(report: dict[str, Any]) -> list[dict[str, Any]]:
    treatment = report["treatment"]
    raw = report["fresh_raw"]
    checks = [
        _check(
            "historical_overall",
            treatment["overall"]["llm_score"] > 0.4065,
            treatment["overall"]["llm_score"],
            ">",
            0.4065,
        ),
        _check(
            "fresh_raw_overall",
            treatment["overall"]["llm_score"] > raw["overall"]["llm_score"],
            treatment["overall"]["llm_score"],
            ">",
            raw["overall"]["llm_score"],
        ),
        _check(
            "scored_count",
            treatment["overall"]["count"] == 1540
            and raw["overall"].get("count") == 1540,
            treatment["overall"]["count"],
            "==",
            1540,
        ),
    ]
    for category, floor in FULL_THRESHOLDS.items():
        actual = treatment["by_category"].get(category, {}).get("llm_score")
        checks.append(
            _check(
                f"category_{category}",
                actual is not None and actual >= floor,
                actual,
                ">=",
                floor,
            )
        )
    checks.append(
        _check(
            "regression_suite",
            bool(report.get("verification", {}).get("regression_passed")),
            bool(report.get("verification", {}).get("regression_passed")),
            "==",
            True,
        )
    )
    return checks


def pilot_checks(report: dict[str, Any]) -> list[dict[str, Any]]:
    raw = report["fresh_raw"]
    treatment = report["treatment"]
    pipeline = report.get("pipeline", {})
    retrieval = report.get("retrieval", {}).get("treatment", {}).get("overall", {})
    raw_score = raw["overall"]["llm_score"]
    treatment_score = treatment["overall"]["llm_score"]
    checks = [
        _check(
            "treatment_beats_raw",
            treatment_score > raw_score,
            treatment_score,
            ">",
            raw_score,
        )
    ]
    shared_categories = sorted(
        set(raw.get("by_category", {})) & set(treatment.get("by_category", {})),
        key=int,
    )
    for category in shared_categories:
        delta = (
            treatment["by_category"][category]["llm_score"]
            - raw["by_category"][category]["llm_score"]
        )
        checks.append(
            _check(f"category_{category}_delta", delta >= -0.05, delta, ">=", -0.05)
        )
    window_count = int(pipeline.get("window_count") or 0)
    empty_windows = int(pipeline.get("empty_extraction_windows") or 0)
    raw_candidates = int(pipeline.get("raw_candidate_count") or 0)
    quarantined = int(pipeline.get("quarantined_count") or 0)
    supported = int(
        (pipeline.get("grounding_status_counts") or {}).get("SUPPORTED") or 0
    )
    health = [
        ("candidate_source_coverage", float(pipeline.get("candidate_source_coverage") or 0.0) == 1.0, pipeline.get("candidate_source_coverage"), "==", 1.0),
        ("window_count", window_count > 0, window_count, ">", 0),
        ("nonempty_windows", window_count > 0 and empty_windows < window_count, empty_windows, "<", window_count),
        ("accepted_memory_count", int(pipeline.get("accepted_memory_count") or 0) > 0, pipeline.get("accepted_memory_count"), ">", 0),
        ("quarantine_not_total", raw_candidates > 0 and quarantined < raw_candidates, quarantined, "<", raw_candidates),
        ("supported_grounding", supported > 0, supported, ">", 0),
        ("evidence_expansion", float(retrieval.get("avg_expanded_source_turns") or 0.0) > 0.0, retrieval.get("avg_expanded_source_turns"), ">", 0.0),
    ]
    checks.extend(_check(name, passed, actual, operator, threshold) for name, passed, actual, operator, threshold in health)
    return checks


def _arm_summary(judged: dict) -> dict[str, Any]:
    full_items = _items(judged)
    category_scores, overall = aggregate_scores(full_items)
    heldout_items = _items(judged, excluded_samples={"0"})
    heldout = _aggregate_or_empty(heldout_items)
    return {
        "overall": overall,
        "by_category": {str(key): value for key, value in category_scores.items()},
        "held_out": heldout,
    }


def _aggregate_or_empty(items: list[dict[str, Any]]) -> dict[str, Any]:
    if not items:
        return {"overall": {"count": 0}, "by_category": {}}
    categories, overall = aggregate_scores(items)
    return {
        "overall": overall,
        "by_category": {str(key): value for key, value in categories.items()},
    }


def _items(judged: dict, excluded_samples: set[str] | None = None) -> list[dict]:
    excluded = excluded_samples or set()
    return [
        item
        for sample_id, sample_items in judged.items()
        if str(sample_id) not in excluded
        for item in sample_items
    ]


def _ordered_query_ids(judged: dict) -> list[str]:
    query_ids = []
    for sample_id in sorted(judged, key=_sample_sort_key):
        sample_items = judged[sample_id]
        for item in sample_items:
            query_id = str(item.get("query_id") or "")
            if not query_id:
                raise ValueError("paired query ids are missing")
            query_ids.append(query_id)
    return query_ids


def _sample_sort_key(value: Any) -> tuple[int, Any]:
    text = str(value)
    return (0, int(text)) if text.isdigit() else (1, text)


def _object_hash(value: Any) -> str:
    payload = json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def _score_delta(raw: dict, treatment: dict) -> dict[str, Any]:
    category_delta = {}
    for category in sorted(
        set(raw.get("by_category", {})) | set(treatment.get("by_category", {})),
        key=int,
    ):
        raw_score = raw.get("by_category", {}).get(category, {}).get("llm_score")
        treatment_score = treatment.get("by_category", {}).get(category, {}).get("llm_score")
        category_delta[category] = (
            treatment_score - raw_score
            if raw_score is not None and treatment_score is not None
            else None
        )
    raw_heldout = raw["held_out"]["overall"].get("llm_score")
    treatment_heldout = treatment["held_out"]["overall"].get("llm_score")
    return {
        "overall_llm_score": treatment["overall"]["llm_score"] - raw["overall"]["llm_score"],
        "by_category_llm_score": category_delta,
        "held_out_llm_score": (
            treatment_heldout - raw_heldout
            if raw_heldout is not None and treatment_heldout is not None
            else None
        ),
    }


def _cost_summary(raw_judged, treatment_judged, pipeline_stats, config):
    raw_items = _items(raw_judged)
    treatment_items = _items(treatment_judged)
    total_tokens = sum(
        int(item.get("total_tokens") or 0)
        + int(item.get("judge_total_tokens") or 0)
        for item in [*raw_items, *treatment_items]
    ) + int(pipeline_stats.get("extraction_total_tokens") or 0) + int(
        pipeline_stats.get("verification_total_tokens") or 0
    )
    cost = config.get("estimated_cost_usd")
    return {
        "reported_total_tokens": total_tokens,
        "raw_answer_latency_seconds": sum(
            float(item.get("response_time") or 0.0) for item in raw_items
        ),
        "raw_judge_latency_ms": sum(
            float(item.get("judge_latency_ms") or 0.0) for item in raw_items
        ),
        "treatment_answer_latency_seconds": sum(
            float(item.get("response_time") or 0.0) for item in treatment_items
        ),
        "treatment_judge_latency_ms": sum(
            float(item.get("judge_latency_ms") or 0.0) for item in treatment_items
        ),
        "estimated_cost_usd": cost,
        "reason": None if cost is not None else "provider cost was not available",
    }


def _check(name, passed, actual, operator, threshold):
    return {
        "name": name,
        "passed": bool(passed),
        "actual": actual,
        "operator": operator,
        "threshold": threshold,
    }


def maybe_write_history_record(path: Path, report: dict[str, Any]) -> bool:
    """Write the common pair records for a complete full LoCoMo comparison."""
    if not _is_complete_full(report):
        remove_stale_history_artifact(path)
        return False
    _write_json_atomic(path, _common_history_records(report))
    return True


def _is_complete_full(report: dict[str, Any]) -> bool:
    return (
        report.get("phase") == "full"
        and (report.get("fresh_raw", {}).get("overall") or {}).get("count") == 1540
        and (report.get("treatment", {}).get("overall") or {}).get("count") == 1540
        and report.get("arm_contract", {}).get("scored_query_count") == 1540
    )


def _common_history_records(report: dict[str, Any]) -> list[dict[str, Any]]:
    configurations = report.get("configuration")
    if not isinstance(configurations, dict):
        raise ValueError("LoCoMo comparison configuration is missing")
    raw_config = configurations.get("fresh_raw")
    extracted_config = configurations.get("treatment")
    if not isinstance(raw_config, dict) or not isinstance(extracted_config, dict):
        raise ValueError("LoCoMo comparison arm configurations are missing")
    raw_run_dir = _required_text(raw_config, "run_dir")
    extracted_run_dir = _required_text(extracted_config, "run_dir")
    raw_dataset = _required_text(raw_config, "dataset")
    if raw_dataset != _required_text(extracted_config, "dataset"):
        raise ValueError("LoCoMo raw/extracted dataset mismatch")
    pair_id = raw_config.get("pair_id")
    if pair_id is not None and pair_id != extracted_config.get("pair_id"):
        raise ValueError("LoCoMo raw/extracted pair_id mismatch")
    if not pair_id:
        pair_id = "locomo-" + canonical_sha256(
            {"raw_run_dir": raw_run_dir, "extracted_run_dir": extracted_run_dir}
        )[:16]

    failed_checks = [
        str(check["name"])
        for check in report.get("promotion", {}).get("checks", [])
        if check.get("passed") is not True
    ]
    policy = promotion_policy_manifest()
    common = {
        "dataset": "locomo",
        "split": str(raw_config.get("split") or Path(raw_dataset).stem),
        "phase": "full",
        "pair_id": str(pair_id),
        "complete": True,
        "arm_contract": report["arm_contract"],
        "policy_hash": report.get("policy_hash") or canonical_sha256(policy),
        "fresh_raw": _common_arm(raw_config, report["fresh_raw"], report, "raw"),
        "treatment": _common_arm(
            extracted_config, report["treatment"], report, "extracted"
        ),
        "promotion": {
            "passed": report["promotion"]["passed"],
            "reasons": failed_checks,
        },
    }
    return build_history_records(common)


def _common_arm(
    config: dict[str, Any],
    qa: dict[str, Any],
    report: dict[str, Any],
    memory_mode: str,
) -> dict[str, Any]:
    run_dir = _required_text(config, "run_dir")
    run_id = config.get("run_id") or (
        f"locomo-{memory_mode}-" + canonical_sha256({"run_dir": run_dir})[:16]
    )
    compact_keys = (
        "chat_model",
        "embedding_model",
        "embedding_dimensions",
        "candidate_k",
        "rerank_model",
        "rerank_input_k",
        "top_k",
        "max_candidate_tokens",
        "max_window_tokens",
    )
    metrics = {
        "qa": {
            "overall": dict(qa["overall"]),
            "by_category": {
                str(category): dict(values)
                for category, values in qa.get("by_category", {}).items()
            },
        },
        "retrieval": dict(report.get("retrieval", {}).get(
            "fresh_raw" if memory_mode == "raw" else "treatment", {}
        )),
    }
    return {
        "run_id": str(run_id),
        "configuration": {
            key: config[key]
            for key in compact_keys
            if key in config and config[key] is not None
        },
        "metrics": metrics,
        "artifact_path": run_dir,
    }


def _required_text(value: dict[str, Any], key: str) -> str:
    result = value.get(key)
    if not isinstance(result, (str, Path)) or not str(result):
        raise ValueError(f"LoCoMo config requires {key}")
    return str(result)


def write_html_report(path: Path, report: dict[str, Any]) -> None:
    overall_rows = [
        (label, arm["overall"].get("llm_score"), arm["overall"].get("count"))
        for label, arm in (
            ("historical", report["historical"]),
            ("fresh raw", report["fresh_raw"]),
            ("treatment", report["treatment"]),
        )
    ]
    heldout_rows = [
        (
            "fresh raw",
            report["fresh_raw"]["held_out"]["overall"].get("llm_score"),
            report["fresh_raw"]["held_out"]["overall"].get("count"),
        ),
        (
            "treatment",
            report["treatment"]["held_out"]["overall"].get("llm_score"),
            report["treatment"]["held_out"]["overall"].get("count"),
        ),
        ("treatment - raw", report["delta"].get("held_out_llm_score"), "—"),
    ]
    categories = sorted(
        set(report["historical"].get("by_category", {}))
        | set(report["fresh_raw"].get("by_category", {}))
        | set(report["treatment"].get("by_category", {})),
        key=int,
    )
    category_rows = [
        (
            category,
            report["historical"].get("by_category", {}).get(category, {}).get("llm_score"),
            report["fresh_raw"].get("by_category", {}).get(category, {}).get("llm_score"),
            report["treatment"].get("by_category", {}).get(category, {}).get("llm_score"),
            report["delta"].get("by_category_llm_score", {}).get(category),
        )
        for category in categories
    ]
    retrieval_rows = []
    retrieval = report.get("retrieval", {})
    retrieval_metrics = sorted(
        {
            key
            for arm in retrieval.values()
            for key in (arm.get("overall", arm) if isinstance(arm, dict) else {})
        }
    )
    for metric in retrieval_metrics:
        retrieval_rows.append(
            (
                metric,
                _metric_value(retrieval.get("historical"), metric),
                _metric_value(retrieval.get("fresh_raw"), metric),
                _metric_value(retrieval.get("treatment"), metric),
            )
        )
    pipeline_rows = [(key, value) for key, value in sorted(report.get("pipeline", {}).items())]
    cost_rows = [(key, value) for key, value in sorted(report.get("cost", {}).items())]
    gate_rows = [
        (
            check["name"],
            check["actual"],
            f"{check['operator']} {check['threshold']}",
            "PASS" if check["passed"] else "FAIL",
        )
        for check in report["promotion"]["checks"]
    ]
    generate_report(
        output_path=str(path),
        title="RAM-A LoCoMo Atomic Memory A/B",
        header_meta={
            "Phase": report["phase"],
            "Promotion": "PASS" if report["promotion"]["passed"] else "FAIL",
        },
        scorecard_html="",
        sections=[
            {
                "title": "Overall Scores",
                "html": _html_table(("Arm", "LLM score", "Count"), overall_rows),
            },
            {
                "title": "Held-out Scores",
                "html": _html_table(("Arm", "LLM score", "Count"), heldout_rows),
            },
            {
                "title": "Category Scores and Deltas",
                "html": _html_table(
                    ("Category", "Historical", "Fresh raw", "Treatment", "Delta"),
                    category_rows,
                ),
            },
            {
                "title": "Retrieval Diagnostics",
                "html": _html_table(
                    ("Metric", "Historical", "Fresh raw", "Treatment"),
                    retrieval_rows,
                ),
            },
            {
                "title": "Pipeline Health",
                "html": _html_table(("Metric", "Value"), pipeline_rows),
            },
            {
                "title": "Token, Latency and Cost",
                "html": _html_table(("Metric", "Value"), cost_rows),
            },
            {
                "title": "Promotion Gate",
                "html": _html_table(
                    ("Check", "Actual", "Required", "Verdict"), gate_rows
                ),
            },
            {
                "title": "Complete JSON",
                "html": f"<pre>{html_escape(json.dumps(report, ensure_ascii=False, indent=2))}</pre>",
            },
        ],
        warnings=[] if report["promotion"]["passed"] else ["Promotion gate did not pass."],
        run_meta=report.get("configuration", {}),
    )


def _metric_value(arm: Any, metric: str) -> Any:
    if not isinstance(arm, dict):
        return None
    values = arm.get("overall", arm)
    return values.get(metric) if isinstance(values, dict) else None


def _html_table(headers: Sequence[str], rows: Sequence[Sequence[Any]]) -> str:
    header_html = "".join(f"<th>{html_escape(value)}</th>" for value in headers)
    body_html = "".join(
        "<tr>" + "".join(f"<td>{html_escape(value)}</td>" for value in row) + "</tr>"
        for row in rows
    )
    if not body_html:
        body_html = f"<tr><td colspan=\"{len(headers)}\">No data</td></tr>"
    return (
        '<div class="table-wrap"><table><thead><tr>'
        f"{header_html}</tr></thead><tbody>{body_html}</tbody></table></div>"
    )


def _read_json(path: Path) -> dict:
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


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Compare paired LoCoMo memory arms.")
    parser.add_argument("--phase", choices=("pilot", "full"))
    parser.add_argument("--raw-dir", type=Path)
    parser.add_argument("--treatment-dir", type=Path)
    parser.add_argument("--output-json", type=Path)
    parser.add_argument("--html-report", type=Path)
    parser.add_argument("--policy", type=Path)
    parser.add_argument("--assert-passed", action="store_true")
    parser.add_argument("--input", type=Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.assert_passed:
        if args.input is None:
            raise ValueError("--input is required with --assert-passed")
        return 0 if _read_json(args.input).get("promotion", {}).get("passed") else 1
    required = (args.phase, args.raw_dir, args.treatment_dir, args.output_json, args.html_report)
    if any(value is None for value in required):
        raise ValueError(
            "--phase, --raw-dir, --treatment-dir, --output-json and --html-report are required"
        )
    history_path = resolve_history_artifact_path(args.output_json)
    raw_judged = _read_json(args.raw_dir / "judge_results.json")
    treatment_judged = _read_json(args.treatment_dir / "judge_results.json")
    raw_retrieval = _read_json(args.raw_dir / "retrieval_metrics.json")
    treatment_retrieval = _read_json(args.treatment_dir / "retrieval_metrics.json")
    pipeline_stats = _read_json(args.treatment_dir / "artifacts" / "extraction_stats.json")
    raw_config = _read_json(args.raw_dir / "config.json")
    config = _read_json(args.treatment_dir / "config.json")
    policy_hash = None
    if args.policy is not None:
        policy = _read_json(args.policy)
        if policy != promotion_policy_manifest():
            raise ValueError("LoCoMo promotion policy does not match the frozen policy")
        policy_hash = file_sha256(args.policy)
        if any(
            arm.get("promotion_policy_hash") != policy_hash
            for arm in (raw_config, config)
        ):
            raise ValueError("promotion policy hash does not match paired configs")
    raw_prepared = _read_json(args.raw_dir / "raw_prepared.json")
    treatment_prepared = _read_json(args.treatment_dir / "raw_prepared.json")
    contract = validate_arm_contract(
        raw_config,
        config,
        raw_prepared,
        treatment_prepared,
        raw_judged,
        treatment_judged,
    )
    report = build_comparison(
        args.phase,
        raw_judged,
        treatment_judged,
        raw_retrieval,
        treatment_retrieval,
        pipeline_stats,
        config,
    )
    report["arm_contract"] = contract
    report["configuration"] = {
        "fresh_raw": raw_config,
        "treatment": config,
    }
    report["policy_hash"] = policy_hash or config.get("promotion_policy_hash")
    report["complete"] = _is_complete_full(report)
    _write_json_atomic(args.output_json, report)
    write_html_report(args.html_report, report)
    maybe_write_history_record(
        history_path,
        report,
    )
    if args.phase == "pilot" and report["promotion"]["passed"]:
        _write_json_atomic(args.output_json.parent / "frozen_config.json", config)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

from __future__ import annotations

import pytest

from locomo.locomo_compare import (
    FULL_THRESHOLDS,
    build_comparison,
    pilot_checks,
    promotion_checks,
    validate_arm_contract,
    validate_paired_query_ids,
    write_html_report,
)


def _full_report(overall: float = 0.4065) -> dict:
    return {
        "phase": "full",
        "fresh_raw": {"overall": {"llm_score": 0.40, "count": 1540}},
        "treatment": {
            "overall": {"llm_score": overall, "count": 1540},
            "by_category": {
                "1": {"llm_score": 0.2199},
                "2": {"llm_score": 0.4361},
                "3": {"llm_score": 0.2917},
                "4": {"llm_score": 0.4709},
            },
        },
        "verification": {"regression_passed": True},
    }


def test_full_gate_uses_strict_overall_and_exact_category_thresholds() -> None:
    checks = {item["name"]: item for item in promotion_checks(_full_report())}

    assert checks["historical_overall"]["passed"] is False
    assert checks["fresh_raw_overall"]["passed"] is True
    assert checks["scored_count"]["passed"] is True
    assert all(checks[f"category_{category}"]["passed"] for category in FULL_THRESHOLDS)

    passing = promotion_checks(_full_report(overall=0.4066))
    assert all(item["passed"] for item in passing)


def test_pilot_rejects_category_drop_below_point_zero_five() -> None:
    report = {
        "phase": "pilot",
        "fresh_raw": {
            "overall": {"llm_score": 0.40},
            "by_category": {"1": {"llm_score": 0.50}},
        },
        "treatment": {
            "overall": {"llm_score": 0.41},
            "by_category": {"1": {"llm_score": 0.4499}},
        },
        "pipeline": {
            "candidate_source_coverage": 1.0,
            "window_count": 2,
            "empty_extraction_windows": 0,
            "accepted_memory_count": 2,
            "raw_candidate_count": 2,
            "quarantined_count": 0,
            "grounding_status_counts": {"SUPPORTED": 2},
        },
        "retrieval": {
            "treatment": {"overall": {"avg_expanded_source_turns": 1.0}}
        },
    }

    checks = {item["name"]: item for item in pilot_checks(report)}

    assert checks["treatment_beats_raw"]["passed"] is True
    assert checks["category_1_delta"]["passed"] is False
    assert all(
        item["passed"]
        for name, item in checks.items()
        if name != "category_1_delta"
    )


def test_paired_query_validation_rejects_duplicates_or_different_order() -> None:
    raw = {"0": [{"query_id": "S0:Q0"}, {"query_id": "S0:Q1"}]}
    duplicate = {"0": [{"query_id": "S0:Q0"}, {"query_id": "S0:Q0"}]}
    reversed_treatment = {
        "0": [{"query_id": "S0:Q1"}, {"query_id": "S0:Q0"}]
    }

    with pytest.raises(ValueError, match="paired query ids"):
        validate_paired_query_ids(raw, duplicate)
    with pytest.raises(ValueError, match="paired query order"):
        validate_paired_query_ids(raw, reversed_treatment)


def test_paired_query_validation_ignores_parallel_group_completion_order() -> None:
    raw = {
        "0": [{"query_id": "S0:Q0"}],
        "1": [{"query_id": "S1:Q0"}],
    }
    treatment = {
        "1": [{"query_id": "S1:Q0"}],
        "0": [{"query_id": "S0:Q0"}],
    }

    validate_paired_query_ids(raw, treatment)


def test_build_comparison_reports_full_heldout_and_cost_unavailable() -> None:
    raw = {
        "0": [_judged("S0:Q0", 1, 0)],
        "1": [_judged("S1:Q0", 1, 1)],
    }
    treatment = {
        "0": [_judged("S0:Q0", 1, 1)],
        "1": [_judged("S1:Q0", 1, 1)],
    }
    pipeline = {
        "candidate_source_coverage": 1.0,
        "window_count": 2,
        "empty_extraction_windows": 0,
        "accepted_memory_count": 2,
        "raw_candidate_count": 2,
        "quarantined_count": 0,
        "grounding_status_counts": {"SUPPORTED": 2},
    }

    report = build_comparison(
        "pilot",
        raw,
        treatment,
        {"overall": {"evidence_hit_at_k": 0.5}},
        {"overall": {"avg_expanded_source_turns": 1.0}},
        pipeline,
        {"configuration_hash": "abc", "regression_passed": True},
    )

    assert report["fresh_raw"]["overall"]["llm_score"] == 0.5
    assert report["treatment"]["overall"]["llm_score"] == 1.0
    assert report["fresh_raw"]["held_out"]["overall"]["count"] == 1
    assert report["promotion"]["passed"] is True
    assert report["cost"]["reported_total_tokens"] == 60
    assert report["cost"]["estimated_cost_usd"] is None
    assert report["cost"]["reason"] == "provider cost was not available"


def test_arm_contract_requires_matching_config_and_authoritative_scored_queries() -> None:
    queries = [
        {"id": "S0:Q0", "task": {"category": 1}},
        {"id": "S0:Q1", "task": {"category": 5}},
    ]
    raw_prepared = {"schema_version": "benchmark-prepared-v1", "queries": queries}
    treatment_prepared = json_clone(raw_prepared)
    raw_judged = {"0": [{"query_id": "S0:Q0"}]}
    treatment_judged = {"0": [{"query_id": "S0:Q0"}]}
    raw_config = {
        "memory_mode": "raw",
        "source_hash": "source",
        "configuration_hash": "config",
        "implementation_hash": "impl",
        "preflight_hash": "preflight",
    }
    treatment_config = {**raw_config, "memory_mode": "extracted"}

    contract = validate_arm_contract(
        raw_config,
        treatment_config,
        raw_prepared,
        treatment_prepared,
        raw_judged,
        treatment_judged,
    )
    assert contract["scored_query_count"] == 1
    assert contract["query_count"] == 2

    treatment_config["configuration_hash"] = "changed"
    with pytest.raises(ValueError, match="configuration hash"):
        validate_arm_contract(
            raw_config,
            treatment_config,
            raw_prepared,
            treatment_prepared,
            raw_judged,
            treatment_judged,
        )

    treatment_config["configuration_hash"] = "config"
    with pytest.raises(ValueError, match="authoritative scored query ids"):
        validate_arm_contract(
            raw_config,
            treatment_config,
            raw_prepared,
            treatment_prepared,
            {"0": []},
            {"0": []},
        )


def test_html_report_contains_separate_audit_tables(tmp_path) -> None:
    raw = {"0": [_judged("S0:Q0", 1, 0)]}
    treatment = {"0": [_judged("S0:Q0", 1, 1)]}
    pipeline = {
        "candidate_source_coverage": 1.0,
        "window_count": 2,
        "accepted_memory_count": 1,
        "quarantined_count": 0,
    }
    report = build_comparison(
        "pilot",
        raw,
        treatment,
        {"overall": {"evidence_hit_at_k": 0.25, "evidence_mrr": 0.125}},
        {
            "overall": {
                "evidence_hit_at_k": 0.5,
                "evidence_mrr": 0.25,
                "avg_expanded_source_turns": 1.5,
            }
        },
        pipeline,
        {"configuration_hash": "abc", "regression_passed": True},
    )
    output = tmp_path / "comparison.html"

    write_html_report(output, report)

    html = output.read_text(encoding="utf-8")
    for heading in (
        "Overall Scores",
        "Held-out Scores",
        "Category Scores and Deltas",
        "Retrieval Diagnostics",
        "Pipeline Health",
        "Token, Latency and Cost",
        "Promotion Gate",
    ):
        assert heading in html
    assert "0.125" in html
    assert "reported_total_tokens" in html

def _judged(query_id: str, category: int, score: int) -> dict:
    return {
        "query_id": query_id,
        "question": "q",
        "answer": "a",
        "response": "a",
        "category": str(category),
        "bleu_score": float(score),
        "f1_score": float(score),
        "llm_score": score,
        "prompt_tokens": 10,
        "completion_tokens": 2,
        "total_tokens": 12,
        "response_time": 0.1,
        "judge_prompt_tokens": 2,
        "judge_completion_tokens": 1,
        "judge_total_tokens": 3,
        "judge_latency_ms": 5.0,
    }


def json_clone(value):
    import json

    return json.loads(json.dumps(value))

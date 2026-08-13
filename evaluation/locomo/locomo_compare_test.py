from __future__ import annotations

import pytest

from locomo.locomo_compare import (
    FULL_THRESHOLDS,
    build_comparison,
    main,
    maybe_write_history_record,
    promotion_checks,
    validate_arm_contract,
    validate_paired_query_ids,
    write_html_report,
    _matching_mode,
    _validate_governance_mode,
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


def test_governance_mode_is_explicit_and_must_match_policy() -> None:
    raw = {"mode": "normal"}
    treatment = {"mode": "normal"}

    mode = _matching_mode(raw, treatment)
    _validate_governance_mode(mode, None)

    with pytest.raises(ValueError, match="must not receive"):
        _validate_governance_mode(mode, {})


def test_governance_mode_rejects_mismatched_or_unreviewed_pairs() -> None:
    with pytest.raises(ValueError, match="mode mismatch"):
        _matching_mode({"mode": "normal"}, {"mode": "strict"})
    with pytest.raises(ValueError, match="requires a promotion policy"):
        _validate_governance_mode("strict", None)


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
        "full",
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
    assert report["promotion"]["passed"] is False
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


def test_normal_arm_contract_allows_missing_preflight_hash() -> None:
    queries = [{"id": "S0:Q0", "task": {"category": 1}}]
    prepared = {"schema_version": "benchmark-prepared-v1", "queries": queries}
    judged = {"0": [{"query_id": "S0:Q0"}]}
    raw_config = {
        "memory_mode": "raw",
        "mode": "normal",
        "source_hash": "source",
        "configuration_hash": "config",
        "implementation_hash": "impl",
        "preflight_hash": None,
    }
    extracted_config = {**raw_config, "memory_mode": "extracted"}

    contract = validate_arm_contract(
        raw_config,
        extracted_config,
        prepared,
        prepared,
        judged,
        judged,
    )

    assert contract["query_count"] == 1


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
        "full",
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


def test_complete_failed_full_pair_writes_common_history_without_mutating_report(
    tmp_path,
) -> None:
    report = _common_history_report(phase="full", count=1540, passed=False)
    before = json_clone(report)
    output = tmp_path / "history_record.json"

    written = maybe_write_history_record(output, report)

    assert written is True
    assert report == before
    records = json_clone_from_path(output)
    assert [record["memory_mode"] for record in records] == ["raw", "extracted"]
    assert records[1]["promotion_status"] == "failed"
    assert records[1]["promotion_reasons"] == ["historical_overall"]
    assert records[0]["schema_version"] == "memory-ab-history-v1"
    assert records[0]["split"] == "locomo10"
    assert records[1]["configuration"]["graph_rerank"] is True
    assert records[1]["configuration"]["graph_allow_graph_only"] is True
    assert records[1]["configuration"]["graph_max_graph_only_results"] == 4


def test_incomplete_pair_does_not_write_common_history(
    tmp_path
) -> None:
    output = tmp_path / "history_record.json"
    output.write_text("stale", encoding="utf-8")

    written = maybe_write_history_record(
        output,
        _common_history_report(phase="full", count=1539, passed=True),
    )

    assert written is False
    assert not output.exists()


def test_cli_rejects_comparison_history_path_collision_before_reading(tmp_path) -> None:
    collision = tmp_path / "history_record.json"

    with pytest.raises(ValueError, match="distinct"):
        main(
            [
                "--phase",
                "full",
                "--raw-dir",
                str(tmp_path / "missing-raw"),
                "--treatment-dir",
                str(tmp_path / "missing-extracted"),
                "--output-json",
                str(collision),
                "--html-report",
                str(tmp_path / "report.html"),
            ]
        )


def _common_history_report(*, phase: str, count: int, passed: bool) -> dict:
    raw_config = {
        "memory_mode": "raw",
        "dataset": "/datasets/locomo10.json",
        "run_dir": "/artifacts/locomo-pair/raw",
        "chat_model": "answer-model",
        "top_k": 30,
        "graph_enabled": True,
        "graph_weight": 0.2,
        "graph_rerank": True,
        "graph_allow_graph_only": True,
        "graph_max_graph_only_results": 4,
        "max_graph_context_facts": 3,
        "rerank_enabled": True,
        "rerank_timeout_ms": 15000,
        "rerank_fail_open": True,
        "source_hash": "source-sha",
        "configuration_hash": "config-sha",
        "implementation_hash": "code-sha",
        "preflight_hash": "preflight-sha",
    }
    extracted_config = {
        **raw_config,
        "memory_mode": "extracted",
        "run_dir": "/artifacts/locomo-pair/extracted",
    }
    arm = {
        "overall": {"llm_score": 0.41, "count": count},
        "by_category": {"1": {"llm_score": 0.21}},
        "held_out": {"overall": {"llm_score": 0.42, "count": count}},
    }
    return {
        "schema_version": "locomo-memory-ab-v1",
        "phase": phase,
        "fresh_raw": arm,
        "treatment": arm,
        "retrieval": {
            "fresh_raw": {"overall": {"evidence_hit_at_k": 0.3}},
            "treatment": {"overall": {"evidence_hit_at_k": 0.4}},
        },
        "cost": {"estimated_cost_usd": None},
        "promotion": {
            "passed": passed,
            "checks": [
                {
                    "name": "historical_overall",
                    "passed": passed,
                    "actual": 0.41,
                    "operator": ">",
                    "threshold": 0.4065,
                }
            ],
        },
        "arm_contract": {
            "source_hash": "source-sha",
            "configuration_hash": "config-sha",
            "implementation_hash": "code-sha",
            "preflight_hash": "preflight-sha",
            "query_count": count,
            "scored_query_count": count,
        },
        "configuration": {
            "fresh_raw": raw_config,
            "treatment": extracted_config,
        },
    }


def json_clone_from_path(path):
    import json

    return json.loads(path.read_text(encoding="utf-8"))

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

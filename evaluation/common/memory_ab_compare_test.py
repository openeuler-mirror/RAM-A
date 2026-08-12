from __future__ import annotations

import pytest

from common.memory_ab_compare import (
    PromotionPolicy,
    build_history_record,
    build_history_records,
    evaluate_checks,
)


def _comparison(*, passed: bool = True, phase: str = "full", complete: bool = True) -> dict:
    return {
        "dataset": "example",
        "split": "test",
        "phase": phase,
        "pair_id": "pair-1",
        "complete": complete,
        "arm_contract": {
            "source_hash": "source-sha",
            "configuration_hash": "config-sha",
            "implementation_hash": "code-sha",
            "preflight_hash": "preflight-sha",
        },
        "policy_hash": "policy-sha",
        "fresh_raw": {
            "run_id": "raw-run",
            "configuration": {"top_k": 10},
            "metrics": {"qa": {"accuracy": 0.72}},
            "artifact_path": "s3://runs/raw-run",
        },
        "treatment": {
            "run_id": "extracted-run",
            "configuration": {"top_k": 10},
            "metrics": {"qa": {"accuracy": 0.73}},
            "artifact_path": "s3://runs/extracted-run",
        },
        "promotion": {
            "passed": passed,
            "reasons": [] if passed else ["fresh_raw_primary"],
        },
    }


def test_full_treatment_must_beat_fresh_raw_and_historical_floor() -> None:
    policy = PromotionPolicy(
        primary_metric="qa.accuracy",
        historical_floor=0.71,
        require_strict_raw_improvement=True,
    )

    report = evaluate_checks(
        policy,
        {"qa": {"accuracy": 0.72}},
        {"qa": {"accuracy": 0.715}},
    )

    assert report["passed"] is False
    assert {c["name"] for c in report["checks"] if not c["passed"]} == {
        "fresh_raw_primary"
    }


def test_completed_failed_promotion_still_builds_history_pair() -> None:
    records = build_history_records(_comparison(passed=False))

    assert [record["memory_mode"] for record in records] == ["raw", "extracted"]
    assert records[1]["promotion_status"] == "failed"


@pytest.mark.parametrize(
    ("raw", "extracted", "message"),
    [
        ({"qa": {}}, {"qa": {"accuracy": 0.8}}, "qa.accuracy"),
        (
            {"qa": {"accuracy": "0.7"}},
            {"qa": {"accuracy": 0.8}},
            "non-numeric",
        ),
    ],
)
def test_metric_paths_reject_missing_or_non_numeric_values(
    raw: dict, extracted: dict, message: str
) -> None:
    policy = PromotionPolicy(primary_metric="qa.accuracy", historical_floor=0.7)

    with pytest.raises(ValueError, match=message):
        evaluate_checks(policy, raw, extracted)


@pytest.mark.parametrize(
    ("phase", "complete"),
    [("full", False)],
)
def test_history_records_require_a_completed_full_pair(
    phase: str, complete: bool
) -> None:
    with pytest.raises(ValueError, match="completed full pair"):
        build_history_records(_comparison(phase=phase, complete=complete))


def test_policy_json_round_trip_includes_versioned_explicit_paths() -> None:
    source = {
        "schema_version": "memory-ab-promotion-v1",
        "primary_metric": "qa.overall.accuracy",
        "historical_floor": 0.7,
        "require_strict_raw_improvement": True,
        "category_floors": {"qa.by_type.single.accuracy": 0.6},
        "completeness_counts": {"qa.overall.total": 100},
        "cost_ceilings": {"cost.estimated_usd": 12.5},
    }

    policy = PromotionPolicy.from_dict(source)

    assert policy.to_dict() == source


def test_policy_json_rejects_missing_schema_version() -> None:
    with pytest.raises(ValueError, match="schema_version"):
        PromotionPolicy.from_dict(
            {
                "primary_metric": "qa.overall.accuracy",
                "historical_floor": 0.7,
            }
        )


@pytest.mark.parametrize("value", [0, 1, "false", None])
def test_policy_rejects_non_boolean_strict_raw_flag(value) -> None:
    with pytest.raises(ValueError, match="require_strict_raw_improvement"):
        PromotionPolicy(
            primary_metric="qa.overall.accuracy",
            historical_floor=0.7,
            require_strict_raw_improvement=value,
        )


def test_category_floor_failure_has_stable_auditable_reason() -> None:
    policy = PromotionPolicy(
        primary_metric="qa.overall.accuracy",
        historical_floor=0.7,
        category_floors={"qa.by_type.single.accuracy": 0.65},
    )
    raw = {
        "qa": {
            "overall": {"accuracy": 0.7},
            "by_type": {"single": {"accuracy": 0.64}},
        }
    }
    extracted = {
        "qa": {
            "overall": {"accuracy": 0.71},
            "by_type": {"single": {"accuracy": 0.64}},
        }
    }

    report = evaluate_checks(policy, raw, extracted)

    assert report["reasons"] == ["category_floor:qa.by_type.single.accuracy"]


def test_completeness_counts_require_both_arms_to_be_exact() -> None:
    policy = PromotionPolicy(
        primary_metric="qa.accuracy",
        historical_floor=0.7,
        completeness_counts={"qa.total": 100},
    )
    raw = {"qa": {"accuracy": 0.70, "total": 99}}
    extracted = {"qa": {"accuracy": 0.71, "total": 100}}

    report = evaluate_checks(policy, raw, extracted)

    assert report["reasons"] == ["raw_completeness:qa.total"]


def test_optional_cost_ceiling_checks_extracted_metrics() -> None:
    policy = PromotionPolicy(
        primary_metric="qa.accuracy",
        historical_floor=0.7,
        cost_ceilings={"cost.estimated_usd": 10.0},
    )
    raw = {"qa": {"accuracy": 0.70}, "cost": {"estimated_usd": 20.0}}
    extracted = {
        "qa": {"accuracy": 0.71},
        "cost": {"estimated_usd": 10.01},
    }

    report = evaluate_checks(policy, raw, extracted)

    assert report["reasons"] == ["cost_ceiling:cost.estimated_usd"]


def test_history_record_contains_required_audit_fields() -> None:
    record = build_history_record(_comparison(), "extracted")

    assert record == {
        "schema_version": "memory-ab-history-v1",
        "pair_id": "pair-1",
        "run_id": "extracted-run",
        "dataset": "example",
        "split": "test",
        "memory_mode": "extracted",
        "phase": "full",
        "source_hash": "source-sha",
        "code_hash": "code-sha",
        "configuration_hash": "config-sha",
        "preflight_hash": "preflight-sha",
        "policy_hash": "policy-sha",
        "governance_mode": "strict",
        "configuration": {"top_k": 10},
        "metrics": {"qa": {"accuracy": 0.73}},
        "promotion_status": "passed",
        "promotion_reasons": [],
        "artifact_path": "s3://runs/extracted-run",
    }


def test_history_record_portabilizes_repository_artifact_path() -> None:
    comparison = _comparison()
    comparison["treatment"]["artifact_path"] = (
        "/home/example/RAM-A-gm-service-integration/outputs/memory-ab/pair/extracted"
    )

    record = build_history_record(comparison, "extracted")

    assert record["artifact_path"] == "outputs/memory-ab/pair/extracted"


def test_history_record_portabilizes_nested_retrieval_input_path() -> None:
    comparison = _comparison()
    comparison["treatment"]["metrics"] = {
        "retrieval": {
            "input": "/home/user/repo/outputs/memory-ab/pair/extracted/search_results.json",
            "query": "keep this text",
        }
    }

    record = build_history_record(comparison, "extracted")

    assert (
        record["metrics"]["retrieval"]["input"]
        == "outputs/memory-ab/pair/extracted/search_results.json"
    )
    assert record["metrics"]["retrieval"]["query"] == "keep this text"


def test_history_record_does_not_rewrite_unrelated_input_text() -> None:
    comparison = _comparison()
    comparison["treatment"]["metrics"] = {
        "answer": {"input": "/home/user/outputs/not-a-path-field"},
        "retrieval": {"query": "keep this text"},
    }

    record = build_history_record(comparison, "extracted")

    assert record["metrics"]["answer"]["input"] == "/home/user/outputs/not-a-path-field"


def test_history_record_uses_last_outputs_component() -> None:
    comparison = _comparison()
    comparison["treatment"]["artifact_path"] = (
        "/home/user/outputs/archive/outputs/memory-ab/pair/extracted"
    )

    record = build_history_record(comparison, "extracted")

    assert record["artifact_path"] == "outputs/memory-ab/pair/extracted"


def test_history_record_rejects_missing_required_value() -> None:
    comparison = _comparison()
    comparison["treatment"]["artifact_path"] = None

    with pytest.raises(ValueError, match="artifact_path"):
        build_history_record(comparison, "extracted")


def test_history_record_rejects_non_boolean_promotion_result() -> None:
    comparison = _comparison()
    comparison["promotion"]["passed"] = "yes"

    with pytest.raises(ValueError, match="promotion passed"):
        build_history_record(comparison, "extracted")


def test_failed_history_record_requires_auditable_reasons() -> None:
    comparison = _comparison(passed=False)
    comparison["promotion"]["reasons"] = []

    with pytest.raises(ValueError, match="promotion reasons"):
        build_history_record(comparison, "extracted")

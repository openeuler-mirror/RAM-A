from __future__ import annotations

import json

import pytest

from common.memory_ab import file_sha256
from common.memory_ab_compare import PromotionPolicy
from personalmem.compare import build_comparison, main


def _config(mode: str) -> dict:
    return {
        "dataset": "personalmem",
        "phase": "full",
        "memory_mode": mode,
        "pair_id": "persona-pair",
        "run_id": f"persona-{mode}",
        "run_dir": f"/artifacts/persona-{mode}",
        "source_hash": "source-sha",
        "configuration_hash": "config-sha",
        "implementation_hash": "code-sha",
        "preflight_hash": "preflight-sha",
        "promotion_policy_hash": "policy-sha",
        "top_k": 10,
        "answer_model": "answer-model",
        "graph": True,
        "graph_build": True,
        "graph_weight": 0.2,
        "graph_rerank": True,
        "graph_allow_graph_only": True,
        "graph_max_graph_only_results": 4,
        "max_graph_context_facts": 3,
        "rerank": True,
        "rerank_timeout_ms": 15000,
        "rerank_fail_open": True,
    }


def _prepared() -> dict:
    return {
        "schema_version": "benchmark-prepared-v1",
        "dataset": {"name": "personalmem", "split": "32k"},
        "queries": [
            {"id": "q1", "text": "one"},
            {"id": "q2", "text": "two"},
        ],
    }


def _grades(accuracy: float, *, total: int = 2) -> dict:
    correct = round(accuracy * total)
    return {
        "report_type": "grade",
        "summary": {
            "total": total,
            "correct": correct,
            "answer_acc": accuracy,
            "valid_predictions": total,
        },
        "by_question_type": [
            {
                "name": "preference",
                "total": total,
                "correct": correct,
                "accuracy": accuracy,
            }
        ],
        "per_query": [{"question_id": "q1", "is_correct": True}],
    }


def _policy() -> PromotionPolicy:
    return PromotionPolicy(
        primary_metric="qa.overall.accuracy",
        historical_floor=0.7,
        category_floors={"qa.by_question_type.preference.accuracy": 0.7},
        completeness_counts={"qa.overall.count": 2},
    )


def test_contract_is_validated_before_grade_metrics_are_scored() -> None:
    raw_config = _config("raw")
    extracted_config = _config("extracted")
    extracted_config["source_hash"] = "different"

    with pytest.raises(ValueError, match="source_hash mismatch"):
        build_comparison(
            raw_config,
            extracted_config,
            _prepared(),
            _prepared(),
            {},
            {},
            _policy(),
        )


def test_full_comparison_reads_grade_accuracy_and_builds_history_records() -> None:
    report = build_comparison(
        _config("raw"),
        _config("extracted"),
        _prepared(),
        _prepared(),
        _grades(0.75),
        _grades(1.0),
        _policy(),
    )

    assert report["schema_version"] == "memory-ab-comparison-v1"
    assert report["fresh_raw"]["metrics"]["qa"]["overall"] == {
        "accuracy": 0.75,
        "count": 2,
        "correct": 2,
    }
    assert report["treatment"]["metrics"]["qa"]["by_question_type"] == {
        "preference": {"accuracy": 1.0, "count": 2, "correct": 2}
    }
    assert "per_query" not in report["treatment"]["metrics"]
    assert report["promotion"]["passed"] is True
    assert [item["memory_mode"] for item in report["history_records"]] == [
        "raw",
        "extracted",
    ]
    configuration = report["history_records"][1]["configuration"]
    assert configuration["graph_rerank"] is True
    assert configuration["graph_allow_graph_only"] is True
    assert configuration["graph_max_graph_only_results"] == 4
    assert configuration["rerank_timeout_ms"] == 15000


def test_incomplete_full_comparison_does_not_build_history_records() -> None:
    report = build_comparison(
        _config("raw"),
        _config("extracted"),
        _prepared(),
        _prepared(),
        _grades(1.0, total=1),
        _grades(1.0, total=1),
        _policy(),
    )

    assert report["complete"] is False
    assert "history_records" not in report
    assert report["promotion"]["passed"] is False


def test_truncated_prepared_pair_cannot_bypass_policy_completeness() -> None:
    prepared = _prepared()
    prepared["queries"] = prepared["queries"][:1]

    report = build_comparison(
        _config("raw"),
        _config("extracted"),
        prepared,
        prepared,
        _grades(1.0, total=1),
        _grades(1.0, total=1),
        _policy(),
    )

    assert report["complete"] is False
    assert "history_records" not in report


def test_full_pair_without_completeness_policy_cannot_build_history() -> None:
    policy = PromotionPolicy(
        primary_metric="qa.overall.accuracy",
        historical_floor=0.7,
    )

    report = build_comparison(
        _config("raw"),
        _config("extracted"),
        _prepared(),
        _prepared(),
        _grades(0.75),
        _grades(1.0),
        policy,
    )

    assert report["complete"] is False
    assert "history_records" not in report


def test_cli_writes_comparison_and_completed_full_history_artifact(tmp_path) -> None:
    raw_dir, extracted_dir, policy_path = _cli_inputs(tmp_path)
    output = tmp_path / "comparison.json"

    result = main(
        [
            "--raw-dir",
            str(raw_dir),
            "--treatment-dir",
            str(extracted_dir),
            "--policy",
            str(policy_path),
            "--output-json",
            str(output),
        ]
    )

    assert result == 0
    assert json.loads(output.read_text(encoding="utf-8"))["promotion"]["passed"]
    history = json.loads(
        (tmp_path / "history_record.json").read_text(encoding="utf-8")
    )
    assert len(history) == 2


def test_cli_removes_stale_history_for_incomplete_pair(tmp_path) -> None:
    raw_dir, extracted_dir, policy_path = _cli_inputs(
        tmp_path,
        raw_grades=_grades(1.0, total=1),
        extracted_grades=_grades(1.0, total=1),
    )
    output = tmp_path / "comparison.json"
    history = tmp_path / "history_record.json"
    history.write_text("stale", encoding="utf-8")

    main(
        [
            "--raw-dir",
            str(raw_dir),
            "--treatment-dir",
            str(extracted_dir),
            "--policy",
            str(policy_path),
            "--output-json",
            str(output),
        ]
    )

    assert not history.exists()


def test_cli_rejects_comparison_history_path_collision_before_reading(tmp_path) -> None:
    collision = tmp_path / "same.json"

    with pytest.raises(ValueError, match="distinct"):
        main(
            [
                "--raw-dir",
                str(tmp_path / "missing-raw"),
                "--treatment-dir",
                str(tmp_path / "missing-extracted"),
                "--policy",
                str(tmp_path / "missing-policy.json"),
                "--output-json",
                str(collision),
                "--history-record",
                str(collision),
            ]
        )


def _cli_inputs(
    tmp_path,
    *,
    raw_grades: dict | None = None,
    extracted_grades: dict | None = None,
):
    raw_dir = tmp_path / "raw"
    extracted_dir = tmp_path / "extracted"
    raw_dir.mkdir()
    extracted_dir.mkdir()
    policy_path = tmp_path / "policy.json"
    _write_json(policy_path, _policy().to_dict())
    policy_hash = file_sha256(policy_path)
    raw_config = _config("raw")
    extracted_config = _config("extracted")
    raw_config["promotion_policy_hash"] = policy_hash
    extracted_config["promotion_policy_hash"] = policy_hash
    for directory, config, grades in (
        (raw_dir, raw_config, raw_grades or _grades(0.75)),
        (extracted_dir, extracted_config, extracted_grades or _grades(1.0)),
    ):
        _write_json(directory / "config.json", config)
        _write_json(directory / "raw_prepared.json", _prepared())
        _write_json(directory / "grade_metrics.json", grades)
    return raw_dir, extracted_dir, policy_path


def _write_json(path, value) -> None:
    path.write_text(json.dumps(value), encoding="utf-8")

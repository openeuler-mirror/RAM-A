from __future__ import annotations

import json

import pytest

from common.memory_ab import file_sha256
from common.memory_ab_compare import PromotionPolicy
from longmemeval.compare import build_comparison, main


def _config(mode: str) -> dict:
    return {
        "dataset": "longmemeval",
        "phase": "full",
        "memory_mode": mode,
        "pair_id": "lme-pair",
        "run_id": f"lme-{mode}",
        "run_dir": f"/artifacts/lme-{mode}",
        "source_hash": "source-sha",
        "configuration_hash": "config-sha",
        "implementation_hash": "code-sha",
        "preflight_hash": "preflight-sha",
        "promotion_policy_hash": "policy-sha",
        "retrieval_top_k": 10,
        "qa_top_k": 10,
        "search_mode": "hybrid",
        "embedding_weight": 0.7,
        "bm25_weight": 0.3,
        "candidate_k": 150,
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
        "dataset": {"name": "longmemeval", "split": "oracle"},
        "queries": [
            {"id": "q1", "text": "one"},
            {"id": "q2_abs", "text": "two"},
        ],
    }


def _qa(accuracy: float, *, total: int = 2) -> dict:
    return {
        "num_questions": total,
        "overall": {"accuracy": accuracy, "correct": round(accuracy * total), "total": total},
        "by_type": {
            "single-session-user": {
                "accuracy": accuracy,
                "correct": round(accuracy * total),
                "total": total,
            }
        },
        "error_analysis": {"large": "omitted from comparison"},
    }


def _retrieval(recall: float, *, missing: int = 0) -> dict:
    group = {"recall@10": recall, "mrr": recall / 2, "ndcg@10": recall / 3}
    return {
        "num_questions": 1,
        "num_evaluated": 1,
        "num_missing_results": missing,
        "num_abstention_excluded": 1,
        "session": {
            "overall": group,
            "by_type": {"single-session-user": group},
        },
        "turn": {
            "overall": group,
            "by_type": {"single-session-user": group},
        },
    }


def _policy() -> PromotionPolicy:
    return PromotionPolicy(
        primary_metric="qa.overall.accuracy",
        historical_floor=0.7,
        category_floors={"qa.by_type.single-session-user.accuracy": 0.7},
        completeness_counts={
            "qa.overall.count": 2,
            "retrieval.overall.count": 1,
            "retrieval.overall.missing": 0,
        },
    )


def test_contract_is_validated_before_qa_or_retrieval_metrics() -> None:
    raw_config = _config("raw")
    extracted_config = _config("extracted")
    extracted_config["preflight_hash"] = "different"

    with pytest.raises(ValueError, match="preflight_hash mismatch"):
        build_comparison(
            raw_config,
            extracted_config,
            _prepared(),
            _prepared(),
            {},
            {},
            {},
            {},
            _policy(),
        )


def test_full_comparison_reads_qa_and_provenance_retrieval_metrics() -> None:
    report = build_comparison(
        _config("raw"),
        _config("extracted"),
        _prepared(),
        _prepared(),
        _qa(0.75),
        _qa(1.0),
        _retrieval(0.6),
        _retrieval(0.8),
        _policy(),
    )

    metrics = report["treatment"]["metrics"]
    assert metrics["qa"]["overall"] == {
        "accuracy": 1.0,
        "count": 2,
        "correct": 2,
    }
    assert metrics["retrieval"]["session"]["overall"] == {
        "recall_at_10": 0.8
    }
    assert metrics["retrieval"]["turn"]["overall"] == {
        "recall_at_10": 0.8,
        "mrr": 0.4,
    }
    assert metrics["retrieval"]["turn"]["by_type"]["single-session-user"] == {
        "recall_at_10": 0.8,
        "mrr": 0.4,
    }
    assert "ndcg@10" not in metrics["retrieval"]["turn"]["overall"]
    assert report["promotion"]["passed"] is True
    assert len(report["history_records"]) == 2
    configuration = report["history_records"][1]["configuration"]
    assert configuration["search_mode"] == "hybrid"
    assert configuration["candidate_k"] == 150
    assert configuration["graph_rerank"] is True
    assert configuration["graph_allow_graph_only"] is True
    assert configuration["graph_max_graph_only_results"] == 4


def test_missing_retrieval_result_makes_full_pair_incomplete() -> None:
    report = build_comparison(
        _config("raw"),
        _config("extracted"),
        _prepared(),
        _prepared(),
        _qa(1.0),
        _qa(1.0),
        _retrieval(1.0, missing=1),
        _retrieval(1.0, missing=1),
        _policy(),
    )

    assert report["complete"] is False
    assert "history_records" not in report
    assert "incomplete_pair" in report["promotion"]["reasons"]


def test_truncated_prepared_pair_cannot_bypass_policy_completeness() -> None:
    prepared = _prepared()
    prepared["queries"] = prepared["queries"][:1]

    report = build_comparison(
        _config("raw"),
        _config("extracted"),
        prepared,
        prepared,
        _qa(1.0, total=1),
        _qa(1.0, total=1),
        _retrieval(1.0),
        _retrieval(1.0),
        _policy(),
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
        raw_retrieval=_retrieval(1.0, missing=1),
        extracted_retrieval=_retrieval(1.0, missing=1),
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
    raw_retrieval: dict | None = None,
    extracted_retrieval: dict | None = None,
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
    for directory, config, qa, retrieval in (
        (raw_dir, raw_config, _qa(0.75), raw_retrieval or _retrieval(0.6)),
        (
            extracted_dir,
            extracted_config,
            _qa(1.0),
            extracted_retrieval or _retrieval(0.8),
        ),
    ):
        _write_json(directory / "config.json", config)
        _write_json(directory / "raw_prepared.json", _prepared())
        _write_json(directory / "qa_metrics.json", qa)
        _write_json(directory / "metrics.json", retrieval)
    return raw_dir, extracted_dir, policy_path


def _write_json(path, value) -> None:
    path.write_text(json.dumps(value), encoding="utf-8")

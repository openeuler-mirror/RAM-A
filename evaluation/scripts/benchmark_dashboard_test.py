import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from scripts import benchmark_dashboard


def _write_json(path: Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value), encoding="utf-8")


def test_dashboard_reads_locomo_run_outputs_without_source_tree_dependency(tmp_path):
    lme_run = tmp_path / "longmemeval"
    pm_run = tmp_path / "personalmem"
    locomo_run = tmp_path / "locomo" / "ram-a"

    _write_json(lme_run / "qa_metrics_smoke.json", {"overall": {"accuracy": 0.5}, "by_type": {}, "diagnostics": {}})
    _write_json(lme_run / "metrics.json", {"session": {"overall": {}}, "turn": {"overall": {}}})
    _write_json(pm_run / "grade_metrics.json", {"summary": {"answer_acc": 0.5}, "by_question_type": []})
    _write_json(pm_run / "retrieval_metrics.json", {"top_k": 10, "query_count": 1})
    _write_json(
        locomo_run / "qa_metrics.json",
        {"overall": {"llm_score": 0.5, "avg_total_tokens": 100}, "by_category": {}},
    )
    _write_json(
        locomo_run / "retrieval_metrics.json",
        {"overall": {"evidence_hit_at_k": 0.5, "evidence_mrr": 0.25, "avg_retrieved_contexts": 10}},
    )
    (locomo_run / "report.html").write_text("<html>report</html>", encoding="utf-8")
    (locomo_run / "errors.html").write_text("<html>errors</html>", encoding="utf-8")

    dashboard = benchmark_dashboard.build_dashboard(
        argparse.Namespace(
            longmemeval_run=lme_run,
            longmemeval_qa="qa_metrics_smoke.json",
            personalmem_run=pm_run,
            personalmem_retrieval=pm_run / "retrieval_metrics.json",
            locomo_run=locomo_run,
        )
    )

    locomo_row = next(row for row in dashboard["rows"] if row["key"] == "locomo")
    assert locomo_row["report"] == locomo_run / "report.html"
    assert locomo_row["errors"] == locomo_run / "errors.html"
    assert locomo_row["local_score"] == 0.5

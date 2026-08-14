import argparse
import json
import sys
from pathlib import Path

EVALUATION_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(EVALUATION_ROOT))

from common.report import generate_report, render_action_link, render_card, fmt_float, fmt_int, fmt_percent, relative_href
from locomo import locomo_metric, locomo_retrieval

PROJECT_ROOT = Path(__file__).resolve().parents[2]
DASHBOARD_PATH = PROJECT_ROOT / "outputs" / "index.html"


def display_backend_name(value):
    return "RAM-A" if value is None else str(value)


def main():
    parser = argparse.ArgumentParser(description="Generate unified LoCoMo per-run reports.")
    parser.add_argument("--retrieval-json", type=Path)
    parser.add_argument("--qa-json", type=Path)
    parser.add_argument("--run-meta", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--errors-output", type=Path, required=True)
    args = parser.parse_args()

    retrieval = load_optional_json(args.retrieval_json)
    qa = load_optional_json(args.qa_json)
    run_meta = load_optional_json(args.run_meta) or {}

    write_error_report(args.errors_output, retrieval, qa, run_meta)
    write_main_report(args.output, retrieval, qa, run_meta, error_href=args.errors_output.name)
    print(f"LoCoMo main HTML report saved to {args.output}")
    print(f"LoCoMo error HTML report saved to {args.errors_output}")


def load_optional_json(path: Path | None):
    if not path or not path.exists():
        return None
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError) as exc:
        print(f"warning: failed to load optional JSON {path}: {exc}", file=sys.stderr)
        return None


def write_main_report(path: Path, retrieval: dict | None, qa: dict | None, run_meta: dict, error_href: str):
    sections = []
    warnings = []
    scorecards = []

    if retrieval:
        overall = retrieval.get("overall", {})
        scorecards.extend([
            render_card("Evidence Hit@K", fmt_percent(overall.get("evidence_hit_at_k")), "retrieval", score_value=overall.get("evidence_hit_at_k")),
            render_card("Evidence MRR", fmt_float(overall.get("evidence_mrr"), 4), "first evidence rank", score_value=overall.get("evidence_mrr")),
        ])
        sections.append({
            "title": "Retrieval Metrics",
            "html": (
                locomo_retrieval.render_summary_table(overall)
                + "<h3>Retrieval by Category</h3>"
                + locomo_retrieval.render_category_table(retrieval.get("by_category", {}))
            ),
        })
        if not retrieval.get("supported", True):
            warnings.append(retrieval.get("unsupported_reason", "Retrieval evidence diagnostics are unsupported."))

    if qa:
        overall = qa.get("overall", {})
        failures = qa.get("failures", [])
        scorecards.extend([
            render_card("LLM Score", fmt_float(overall.get("llm_score"), 4), "mean judge score", score_value=overall.get("llm_score"), metric_type="binary"),
            render_card("F1", fmt_float(overall.get("f1_score"), 4), "mean token overlap", score_value=overall.get("f1_score")),
            render_card("Answer Tokens", fmt_int(overall.get("avg_total_tokens")), "avg total tokens"),
            render_card("Latency P50/P95", locomo_metric._latency_pair(overall), "seconds"),
        ])
        sections.append({
            "title": "QA / Grade Metrics",
            "html": (
                locomo_metric.render_overall_table(overall)
                + "<h3>Scores by Category</h3>"
                + locomo_metric.render_category_table(qa.get("by_category", {}))
            ),
        })
        if failures or (retrieval and retrieval.get("failures")):
            sections.append({
                "title": "Sample Failures",
                "subtitle": "Shows a compact failure summary. Use the full error report for item-level review.",
                "html": render_action_link(error_href, "View full error report")
                + _summary_failures_html(retrieval, qa),
            })
        if overall.get("skipped_count"):
            warnings.append(
                f"{overall['skipped_count']} category-5 adversarial/unanswerable question(s) "
                "were excluded from the main QA score. This follows memory-system "
                "baseline practice; evaluate them separately with an abstention rubric."
            )

    if not sections:
        sections.append({"title": "Benchmark Summary", "html": '<div class="note">No LoCoMo metrics available.</div>'})

    generate_report(
        output_path=str(path),
        title="RAM-A LoCoMo Report",
        header_meta={
            "Dataset": "LoCoMo",
            "Backend": display_backend_name(run_meta.get("backend")),
            "Top K": run_meta.get("top_k", "unknown"),
        },
        scorecard_html="".join(scorecards),
        sections=sections,
        warnings=warnings,
        run_meta=run_meta,
        back_to_index_href=relative_href(DASHBOARD_PATH, path),
    )


def write_error_report(path: Path, retrieval: dict | None, qa: dict | None, run_meta: dict):
    sections = []
    back_link = render_action_link("report.html", "Back to main report")
    nav_html = back_link
    if retrieval:
        sections.append({
            "title": "Retrieval Missing Evidence",
            "html": nav_html + locomo_retrieval.render_failure_table(retrieval.get("failures", [])),
        })
    if qa:
        qa_failures = _enrich_qa_failures_with_retrieval(retrieval, qa.get("failures", []))
        sections.append({
            "title": "QA / Grade Failures",
            "html": nav_html + locomo_metric.render_failure_table(qa_failures),
        })
    if not sections:
        sections.append({"title": "Failure Details", "html": nav_html + '<div class="note">No failures available.</div>'})
    generate_report(
        output_path=str(path),
        title="RAM-A LoCoMo Error Report",
        header_meta={"Dataset": "LoCoMo", "Backend": display_backend_name(run_meta.get("backend"))},
        scorecard_html="",
        sections=sections,
        run_meta=run_meta,
        show_run_info=False,
    )


def _summary_failures_html(retrieval: dict | None, qa: dict | None) -> str:
    parts = []
    if retrieval:
        parts.append("<h3>Retrieval Examples</h3>" + locomo_retrieval.render_failure_table(retrieval.get("failures", [])[:3]))
    if qa:
        qa_failures = _enrich_qa_failures_with_retrieval(retrieval, qa.get("failures", [])[:3])
        parts.append("<h3>QA Examples</h3>" + locomo_metric.render_failure_table(qa_failures))
    return "".join(parts) or '<div class="note">No failures available.</div>'


def _enrich_qa_failures_with_retrieval(retrieval: dict | None, failures: list[dict]) -> list[dict]:
    if not retrieval or not failures:
        return failures
    lookup = _retrieval_lookup(retrieval)
    enriched = []
    for item in failures:
        row = lookup.get(_qa_key(item))
        if not row:
            enriched.append(item)
            continue
        output = dict(item)
        output["retrieval_evidence_hit"] = row.get("evidence_hit")
        output["retrieval_first_hit_rank"] = row.get("first_hit_rank")
        output["retrieval_evidence_count"] = row.get("evidence_count")
        output["retrieval_context_tokens"] = row.get("context_tokens")
        enriched.append(output)
    return enriched


def _retrieval_lookup(retrieval: dict) -> dict[tuple[str, str], dict]:
    rows = retrieval.get("per_query")
    if not isinstance(rows, list):
        rows = retrieval.get("failures", [])
    lookup = {}
    for row in rows:
        if isinstance(row, dict):
            lookup[_qa_key(row)] = row
    return lookup


def _qa_key(item: dict) -> tuple[str, str]:
    return (str(item.get("category")), str(item.get("question", "")).strip())


if __name__ == "__main__":
    main()

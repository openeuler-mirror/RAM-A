#!/usr/bin/env python3
"""Generate a cross-dataset benchmark dashboard.

The dashboard compares local RAM-A runs with published mem0/memos
baseline numbers from MemTensor/MemOS_eval_result.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

EVALUATION_ROOT = Path(__file__).resolve().parents[1]
PROJECT_ROOT = EVALUATION_ROOT.parent
sys.path.insert(0, str(EVALUATION_ROOT))

from common.report import (  # noqa: E402
    fmt_float,
    fmt_int,
    fmt_percent,
    generate_report,
    html_escape,
    humanize_label,
    render_action_link,
    relative_href,
    score_class,
)

SOURCE_URL = "https://huggingface.co/datasets/MemTensor/MemOS_eval_result"

# Fixed pixel width for the three method columns (RAM-A / mem0 / memos)
# so they stay visually aligned across every comparison table.
METHOD_COL_PX = 132
METHOD_COL = f'<col style="width: {METHOD_COL_PX}px">'
METHOD_COLGROUP = METHOD_COL * 3

# Published external baselines from MemTensor/MemOS_eval_result.
# Source files:
# - LongMemEval: `lme_results/mem0-api_lme_grades.json`,
#   `lme_results/memos-api_lme_grades.json`
# - PersonaMem: `personamem_results/mem0_personamem_result.json`,
#   `personamem_results/memos_personamem_result.json`
# - LoCoMo: `locomo_results/mem0_graph/locomo_metric.json`,
#   `locomo_results/memos/locomo_metric.json`
# Values are stored as fractions where they represent accuracy-like percentages.
EXTERNAL_BASELINES = {
    "longmemeval": {
        "metric": "Overall Accuracy",
        "mem0": {"score": 0.664, "tokens": 1066},
        "memos": {"score": 0.772, "tokens": 890},
    },
    "personalmem": {
        "metric": "4-Option Accuracy",
        "mem0": {"score": 0.4312, "tokens": 140},
        "memos": {"score": 0.6117, "tokens": 1423.93},
    },
    "locomo": {
        "metric": "Overall Accuracy",
        "mem0": {"score": 0.6457, "tokens": 1170, "f1": 0.4346},
        "memos": {"score": 0.7580, "tokens": 2640, "f1": 0.4527},
    },
}

EXTERNAL_BREAKDOWNS = {
    "longmemeval": {
        "Single-session (user)": {"mem0": 0.8285714285714286, "memos": 0.9285714285714286},
        "Single-session (assistant)": {"mem0": 0.26785714285714285, "memos": 0.6785714285714286},
        "Single-session (preference)": {"mem0": 0.9, "memos": 0.8666666666666667},
        "Knowledge update": {"mem0": 0.6666666666666666, "memos": 0.7307692307692307},
        "Temporal reasoning": {"mem0": 0.7218045112781954, "memos": 0.7669172932330827},
        "Multi-session": {"mem0": 0.631578947368421, "memos": 0.7368421052631579},
    },
    "personalmem": {
        "track_full_preference_evolution": {"mem0": 0.45323741007194246, "memos": 0.513189448441247},
        "suggest_new_ideas": {"mem0": 0.19354838709677416, "memos": 0.38351254480286734},
        "recall_user_shared_facts": {"mem0": 0.36950904392764855, "memos": 0.6847545219638244},
        "generalizing_to_new_scenarios": {"mem0": 0.28654970760233917, "memos": 0.6198830409356725},
        "provide_preference_aligned_recommendations": {"mem0": 0.4666666666666666, "memos": 0.6787878787878788},
        "recalling_the_reasons_behind_previous_updates": {"mem0": 0.7710437710437711, "memos": 0.8215488215488215},
        "recalling_facts_mentioned_by_the_user": {"mem0": 0.411764705882353, "memos": 0.6470588235294118},
    },
    "locomo": {
        "Multi hop": {"mem0": 0.5070921985815603, "memos": 0.6749408983451536},
        "Temporal reasoning": {"mem0": 0.4672897196261682, "memos": 0.7518172377985461},
        "Open domain": {"mem0": 0.4479166666666667, "memos": 0.5590277777777778},
        "Single hop": {"mem0": 0.6551724137931034, "memos": 0.8109393579072534},
    },
}

def main() -> int:
    parser = argparse.ArgumentParser(description="Generate the benchmark dashboard HTML.")
    parser.add_argument("--longmemeval-run", type=Path, required=True)
    parser.add_argument("--longmemeval-qa", required=True)
    parser.add_argument("--personalmem-run", type=Path, required=True)
    parser.add_argument("--personalmem-retrieval", type=Path, required=True)
    parser.add_argument("--locomo-run", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=PROJECT_ROOT / "outputs/index.html")
    args = parser.parse_args()

    dashboard = build_dashboard(args)
    write_dashboard(dashboard, args.output)
    print(f"Benchmark dashboard saved to {args.output}")
    return 0


def build_dashboard(args: argparse.Namespace) -> dict[str, Any]:
    lme = load_longmemeval(args.longmemeval_run, args.longmemeval_qa)
    personalmem = load_personalmem(args.personalmem_run, args.personalmem_retrieval)
    locomo = load_locomo(args.locomo_run)
    rows = [lme, personalmem, locomo]
    return {
        "rows": rows,
        "generated_from": {
            "longmemeval": args.longmemeval_run,
            "personalmem": args.personalmem_run,
            "locomo": args.locomo_run,
        },
    }


def load_longmemeval(run_dir: Path, qa_filename: str) -> dict[str, Any]:
    qa = load_json(run_dir / qa_filename)
    retrieval = load_json(run_dir / "metrics.json")
    overall = qa.get("overall", {})
    diagnostics = qa.get("diagnostics", {})
    session = retrieval.get("session", {}).get("overall", {})
    turn = retrieval.get("turn", {}).get("overall", {})
    return {
        "key": "longmemeval",
        "dataset": "LongMemEval",
        "metric": "QA Accuracy",
        "local_score": overall.get("accuracy"),
        "local_tokens": overall.get("avg_context_tokens"),
        "breakdown": normalize_lme_breakdown(qa.get("by_type", {})),
        "retrieval": {
            "Session R@10": session.get("recall@10"),
            "Turn R@10": turn.get("recall@10"),
            "Gold Hit Rate": diagnostics.get("gold_hit_rate"),
        },
        "report": run_dir / "report.html",
        "errors": run_dir / "errors.html",
    }


def load_personalmem(run_dir: Path, retrieval_path: Path) -> dict[str, Any]:
    grade = load_json(run_dir / "grade_metrics.json")
    retrieval = load_json(retrieval_path) if retrieval_path.exists() else {}
    summary = grade.get("summary", {})
    return {
        "key": "personalmem",
        "dataset": "PersonaMem",
        "metric": "4-Option Accuracy",
        "local_score": summary.get("answer_acc"),
        "local_tokens": summary.get("avg_context_tokens"),
        "breakdown": normalize_personalmem_breakdown(grade.get("by_question_type", [])),
        "retrieval": {
            "Top K": retrieval.get("top_k"),
            "Query Count": retrieval.get("query_count"),
            "Scoring": "diagnostic only" if retrieval.get("retrieval_scoring_supported") is False else "supported",
        },
        "report": run_dir / "report.html",
        "errors": run_dir / "errors.html",
    }


def load_locomo(run_dir: Path) -> dict[str, Any]:
    qa = load_json(run_dir / "qa_metrics.json")
    retrieval = load_json(run_dir / "retrieval_metrics.json")
    overall = qa.get("overall", {})
    retr_overall = retrieval.get("overall", {})
    return {
        "key": "locomo",
        "dataset": "LoCoMo",
        "metric": "LLM Judge Score",
        "local_score": overall.get("llm_score"),
        "local_tokens": overall.get("avg_total_tokens"),
        "breakdown": normalize_locomo_breakdown(qa.get("by_category", {})),
        "retrieval": {
            "Evidence Hit@K": retr_overall.get("evidence_hit_at_k"),
            "Evidence MRR": retr_overall.get("evidence_mrr"),
            "Top K": retr_overall.get("avg_retrieved_contexts"),
        },
        "report": run_dir / "report.html",
        "errors": run_dir / "errors.html",
    }


def write_dashboard(dashboard: dict[str, Any], output_path: Path) -> None:
    rows = dashboard["rows"]
    source_link = f'<a href="{html_escape(SOURCE_URL)}">MemTensor/MemOS_eval_result</a>'
    sections = [
        {
            "title": "Benchmark Summary",
            "subtitle": "Primary score comparison. External mem0/memos values are published baseline numbers.",
            "html": render_summary_table(rows, output_path),
        },
        {
            "title": "Token Cost",
            "subtitle": "Average context or total tokens reported by each benchmark source. Token accounting may differ across systems.",
            "html": render_cost_table(rows),
        },
        {
            "title": "RAM-A Breakdown",
            "subtitle": "Local scores by dataset-specific category, following the compact category-table style used in memory benchmark reports.",
            "html": render_breakdown_tables(rows),
        },
        {
            "title": "Local Retrieval Signals",
            "subtitle": "Diagnostic-only signals for RAM-A development. These are not always available in public mem0/memos reports.",
            "html": render_retrieval_table(rows),
        },
        {
            "title": "Sources and Caveats",
            "html": (
                '<div class="note">'
                f"External baseline values are copied from {source_link}. "
                "They are useful as reference targets, but may not use the same prompts, model versions, "
                "token accounting, or retrieval pipeline as the local RAM-A runs. "
                "Use linked per-dataset reports for detailed local diagnostics."
                "</div>"
            ),
        },
    ]
    generate_report(
        output_path=str(output_path),
        title="RAM-A Benchmark Dashboard",
        header_meta={
            "Datasets": len(rows),
            "Local backend": "RAM-A",
            "External baselines": "mem0 / memos",
        },
        scorecard_html="",
        sections=sections,
        run_meta={key: str(value) for key, value in dashboard.get("generated_from", {}).items()},
        show_run_info=False,
    )


def render_summary_table(rows: list[dict[str, Any]], output_path: Path) -> str:
    body = []
    for row in rows:
        baseline = EXTERNAL_BASELINES[row["key"]]
        mem0 = baseline["mem0"]
        memos = baseline["memos"]
        score = row.get("local_score")
        score_cls = score_class(score) if score is not None else ""
        body.append(
            "<tr>"
            f"<td><strong>{html_escape(row['dataset'])}</strong><br><span class=\"subtle\">{html_escape(row['metric'])}</span></td>"
            f'<td class="mono method-col {score_cls}">{fmt_percent(score)}</td>'
            f'<td class="mono method-col">{fmt_percent(mem0["score"])}</td>'
            f'<td class="mono method-col">{fmt_percent(memos["score"])}</td>'
            f'<td class="reports-col">{render_links(row, output_path)}</td>'
            "</tr>"
        )
    return (
        '<div class="table-wrap"><table class="comparison-table">'
        f'<colgroup><col>{METHOD_COLGROUP}<col></colgroup>'
        '<thead><tr>'
        '<th>Dataset</th><th class="mono method-col">RAM-A</th>'
        '<th class="mono method-col">mem0</th><th class="mono method-col">memos</th>'
        '<th class="reports-col">Reports</th>'
        f"</tr></thead><tbody>{''.join(body)}</tbody></table></div>"
    )


def render_cost_table(rows: list[dict[str, Any]]) -> str:
    body = []
    for row in rows:
        baseline = EXTERNAL_BASELINES[row["key"]]
        mem0 = baseline["mem0"]
        memos = baseline["memos"]
        body.append(
            "<tr>"
            f"<td><strong>{html_escape(row['dataset'])}</strong></td>"
            f'<td class="mono method-col">{fmt_int(row.get("local_tokens"))}</td>'
            f'<td class="mono method-col">{fmt_int(mem0.get("tokens"))}</td>'
            f'<td class="mono method-col">{fmt_int(memos.get("tokens"))}</td>'
            "</tr>"
        )
    return (
        '<div class="table-wrap"><table class="comparison-table">'
        f'<colgroup><col>{METHOD_COLGROUP}</colgroup>'
        '<thead><tr>'
        '<th>Dataset</th><th class="mono method-col">RAM-A</th>'
        '<th class="mono method-col">mem0</th><th class="mono method-col">memos</th>'
        f'</tr></thead><tbody>{"".join(body)}</tbody></table></div>'
    )


def render_breakdown_tables(rows: list[dict[str, Any]]) -> str:
    parts = []
    for row in rows:
        body = []
        for item in row.get("breakdown", []):
            score = item.get("score")
            score_cls = score_class(score) if score is not None else ""
            body.append(
                "<tr>"
                f"<td>{html_escape(item.get('category'))}</td>"
                f'<td class="mono method-col {score_cls}">{fmt_percent(score)}</td>'
                f'<td class="mono method-col">{fmt_percent(item.get("mem0"))}</td>'
                f'<td class="mono method-col">{fmt_percent(item.get("memos"))}</td>'
                "</tr>"
            )
        if not body:
            continue
        parts.append(
            '<div class="subsection">'
            f"<h3>{html_escape(row['dataset'])}</h3>"
            '<div class="table-wrap"><table class="comparison-table">'
            f'<colgroup><col>{METHOD_COLGROUP}</colgroup>'
            '<thead><tr><th>Category</th><th class="mono method-col">RAM-A</th>'
            '<th class="mono method-col">mem0</th><th class="mono method-col">memos</th></tr></thead>'
            f'<tbody>{"".join(body)}</tbody></table></div>'
            "</div>"
        )
    return "".join(parts) or '<div class="note">No breakdown metrics available.</div>'


def render_retrieval_table(rows: list[dict[str, Any]]) -> str:
    body = []
    for row in rows:
        diagnostics = row.get("retrieval") or {}
        details = []
        for key, value in diagnostics.items():
            if isinstance(value, float):
                rendered = fmt_percent(value) if 0 <= value <= 1 else fmt_float(value, 2)
            elif isinstance(value, int):
                rendered = fmt_int(value)
            elif value is None:
                rendered = "n/a"
            else:
                rendered = str(value)
            details.append(f"{html_escape(key)}: <span class=\"mono\">{html_escape(rendered)}</span>")
        body.append(
            "<tr>"
            f"<td><strong>{html_escape(row['dataset'])}</strong></td>"
            f"<td>{'<br>'.join(details)}</td>"
            "</tr>"
        )
    return (
        '<div class="table-wrap"><table><thead><tr><th>Dataset</th><th>Diagnostics</th></tr></thead>'
        f"<tbody>{''.join(body)}</tbody></table></div>"
    )


def render_links(row: dict[str, Any], output_path: Path) -> str:
    links = []
    for label, path in (("Report", row.get("report")), ("Errors", row.get("errors"))):
        if isinstance(path, Path) and path.exists():
            links.append(render_action_link(relative_href(path, output_path), label))
    return " ".join(links) if links else '<span class="mono">n/a</span>'


def normalize_lme_breakdown(by_type: dict[str, dict[str, Any]]) -> list[dict[str, Any]]:
    order = [
        "single-session-user",
        "single-session-assistant",
        "single-session-preference",
        "knowledge-update",
        "temporal-reasoning",
        "multi-session",
    ]
    labels = {
        "single-session-user": "Single-session (user)",
        "single-session-assistant": "Single-session (assistant)",
        "single-session-preference": "Single-session (preference)",
        "knowledge-update": "Knowledge update",
        "temporal-reasoning": "Temporal reasoning",
        "multi-session": "Multi-session",
    }
    rows = []
    for key in order:
        values = by_type.get(key)
        if not values:
            continue
        rows.append(
            {
                "category": labels.get(key, key),
                "score": values.get("accuracy"),
                **external_category("longmemeval", labels.get(key, key)),
            }
        )
    return rows


def normalize_personalmem_breakdown(items: list[dict[str, Any]]) -> list[dict[str, Any]]:
    rows = []
    for values in items:
        raw_name = values.get("name")
        rows.append(
            {
                "category": humanize_label(raw_name),
                "score": values.get("accuracy"),
                **external_category("personalmem", raw_name),
            }
        )
    return rows


def normalize_locomo_breakdown(by_category: dict[str, dict[str, Any]]) -> list[dict[str, Any]]:
    rows = []
    labels = {
        "1": "Multi hop",
        "2": "Temporal reasoning",
        "3": "Open domain",
        "4": "Single hop",
    }
    for category, values in sorted(by_category.items(), key=lambda item: int(item[0])):
        label = labels.get(str(category), f"Category {category}")
        rows.append(
            {
                "category": label,
                "score": values.get("llm_score"),
                **external_category("locomo", label),
            }
        )
    return rows


def external_category(dataset_key: str, category: str | None) -> dict[str, float | None]:
    values = EXTERNAL_BREAKDOWNS.get(dataset_key, {}).get(str(category), {})
    return {
        "mem0": values.get("mem0"),
        "memos": values.get("memos"),
    }


def load_json(path: Path) -> dict[str, Any]:
    if not path.exists():
        raise FileNotFoundError(f"required metrics file not found: {path}")
    return json.loads(path.read_text(encoding="utf-8"))


if __name__ == "__main__":
    raise SystemExit(main())

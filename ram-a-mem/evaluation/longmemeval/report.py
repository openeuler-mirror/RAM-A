"""LongMemEval-specific report rendering.

Builds LME sections (Benchmark Summary, QA by Type, Retrieval, Failure Analysis)
and delegates to the generic report framework for HTML assembly.
"""

from __future__ import annotations

from pathlib import Path

from common.report import (
    fmt_int,
    fmt_ms,
    fmt_percent,
    html_escape,
    humanize_label,
    make_bar_chart,
    render_action_link,
    render_card,
    render_metric_value,
    render_text_cell,
    relative_href,
    score_class,
)

try:
    import plotly.graph_objects as go
except ModuleNotFoundError:
    go = None

# ── LME constants ───────────────────────────────────────────────────────────

TYPE_DISPLAY = {
    "single-session-user": "Single-session (user)",
    "single-session-assistant": "Single-session (assistant)",
    "single-session-preference": "Single-session (preference)",
    "knowledge-update": "Knowledge update",
    "temporal-reasoning": "Temporal reasoning",
    "multi-session": "Multi-session",
}

TYPE_ORDER = [
    "single-session-user",
    "single-session-assistant",
    "single-session-preference",
    "knowledge-update",
    "temporal-reasoning",
    "multi-session",
]

_METRIC_ORDER = [
    "recall@1", "recall@3", "recall@5", "recall@10",
    "mrr", "ndcg@5", "ndcg@10",
]

_ERROR_LABEL_MEANING = {
    "retrieval_miss": "Gold evidence was not retrieved in QA top-k",
    "no_gold_reference": "No gold evidence reference was available",
    "over_abstention": "Model abstained despite retrieved gold evidence",
    "counting_error": "Question requires counting or enumeration",
    "date_math_error": "Question requires date, duration, or time arithmetic",
    "ordering_error": "Question requires first/before/after ordering",
    "multi_session_reasoning": "Question likely needs combining multiple sessions",
    "knowledge_update_reasoning": "Question likely needs resolving updated facts",
    "answer_reasoning_error": "Gold was retrieved but no narrower heuristic matched",
    "abstention": "Model abstained and evidence may be missing",
    "judge_or_answer_format": "Potential judge or answer formatting issue",
}

PROJECT_ROOT = Path(__file__).resolve().parents[2]
DASHBOARD_PATH = PROJECT_ROOT / "outputs" / "index.html"


def _display_backend_name(value: str | None) -> str:
    return "RAM-A" if value is None else str(value)


# ── LME scorecard ───────────────────────────────────────────────────────────

def _render_lme_scorecard(metrics: dict, qa_metrics: dict | None) -> str:
    session = metrics.get("session", {}).get("overall", {})
    turn = metrics.get("turn", {}).get("overall", {})
    diagnostics = (qa_metrics or {}).get("diagnostics") or {}
    overall = (qa_metrics or {}).get("overall") or {}

    accuracy = overall.get("accuracy")
    correct = overall.get("correct")
    total = overall.get("total")
    avg_context = overall.get("avg_context_tokens")
    avg_latency = overall.get("avg_total_latency_ms")
    session_r10 = session.get("recall@10")
    turn_r10 = turn.get("recall@10")
    gold_hit = diagnostics.get("gold_hit_rate")

    return "".join([
        render_card("QA Accuracy", fmt_percent(accuracy), f"{correct or 0} / {total or 0}" if qa_metrics else "QA not run", score_value=accuracy),
        render_card("Context Tokens", fmt_int(avg_context), "avg context per query"),
        render_card("Avg Latency", fmt_ms(avg_latency), "answer + judge"),
        render_card("Session R@10", fmt_percent(session_r10), "retrieval", score_value=session_r10),
        render_card("Turn R@10", fmt_percent(turn_r10), "retrieval", score_value=turn_r10),
        render_card("Gold Hit Rate", fmt_percent(gold_hit), "QA top-k evidence", score_value=gold_hit),
    ])


# ── LME warnings ────────────────────────────────────────────────────────────

def _build_lme_warnings(metrics: dict) -> list[str]:
    warnings = []
    missing = metrics.get("num_missing_results", 0)
    abstention_excluded = metrics.get("num_abstention_excluded", 0)
    if missing:
        warnings.append(f"Missing search results: {missing}")
    if abstention_excluded:
        warnings.append(f"Retrieval metrics excluded abstention questions: {abstention_excluded}")
    return warnings


# ── LME Benchmark Summary ───────────────────────────────────────────────────

def _render_lme_benchmark_summary(qa_metrics: dict | None) -> str:
    overall = (qa_metrics or {}).get("overall") or {}
    by_type = (qa_metrics or {}).get("by_type") or {}

    if not qa_metrics:
        return '<div class="note">QA metrics not available for this run.</div>'

    acc = overall.get("accuracy", 0.0)
    tokens = overall.get("avg_context_tokens")
    overall_row = (
        "<tr>"
        f"<td><strong>Overall</strong></td>"
        f'<td class="mono {score_class(acc, metric_type="quality")}">{acc * 100:.1f}%</td>'
        f'<td class="mono">{fmt_int(tokens)}</td>'
        f"</tr>"
    )

    type_rows = []
    for qtype in TYPE_ORDER:
        values = by_type.get(qtype)
        if not values:
            continue
        acc = float(values.get("accuracy", 0.0))
        tokens = values.get("avg_context_tokens")
        display = TYPE_DISPLAY.get(qtype, qtype)
        type_rows.append(
            "<tr>"
            f"<td>{display}</td>"
            f'<td class="mono {score_class(acc, metric_type="quality")}">{acc * 100:.1f}%</td>'
            f'<td class="mono">{fmt_int(tokens)}</td>'
            f"</tr>"
        )

    return (
        '<div class="table-wrap"><table><thead><tr><th>Category</th><th class="mono">Accuracy</th>'
        f'<th class="mono">Context Tokens</th></tr></thead>'
        f'<tbody>{overall_row}{"".join(type_rows)}</tbody></table></div>'
    )


# ── LME QA by Type ──────────────────────────────────────────────────────────

def _render_lme_qa_by_type_table(qa_metrics: dict) -> str:
    by_type = qa_metrics.get("by_type", {})
    rows = []
    for qtype, values in sorted(by_type.items()):
        accuracy = float(values.get("accuracy", 0.0))
        rows.append(
            "<tr>"
            f"<td><strong>{qtype}</strong></td>"
            f'<td class="mono {score_class(accuracy, metric_type="quality")}">{accuracy * 100:.1f}%</td>'
            f'<td class="mono">{values.get("correct", 0)} / {values.get("total", 0)}</td>'
            f'<td class="mono">{float(values.get("avg_total_tokens", 0.0)):.2f}</td>'
            f'<td class="mono">{float(values.get("avg_context_tokens", 0.0)):.2f}</td>'
            f'<td class="mono">{float(values.get("avg_total_latency_ms", 0.0)):.2f}</td>'
            "</tr>"
        )
    return (
        '<div class="table-wrap"><table><thead><tr><th>Type</th><th class="mono">Accuracy</th>'
        '<th class="mono">Correct</th><th class="mono">Avg Tokens</th>'
        '<th class="mono">Context Tokens</th><th class="mono">Latency ms</th></tr></thead>'
        f'<tbody>{"".join(rows)}</tbody></table></div>'
    )


# ── LME Retrieval section ───────────────────────────────────────────────────

def _render_retrieval_overall_table(session_overall: dict, turn_overall: dict) -> str:
    rows = []
    for metric in _METRIC_ORDER:
        s_val = session_overall.get(metric)
        t_val = turn_overall.get(metric)
        rows.append(
            "<tr>"
            f'<td class="mono">{metric}</td>'
            f"<td>{render_metric_value(s_val)}</td>"
            f"<td>{render_metric_value(t_val)}</td>"
            "</tr>"
        )
    return (
        '<div class="table-wrap"><table><thead><tr><th class="mono">Metric</th>'
        '<th class="mono">Session</th><th class="mono">Turn</th></tr></thead>'
        f'<tbody>{"".join(rows)}</tbody></table></div>'
    )


def _render_retrieval_by_type_table(session_by_type: dict, turn_by_type: dict) -> str:
    all_types = sorted(set(session_by_type) | set(turn_by_type))
    rows = []
    for qtype in all_types:
        s = session_by_type.get(qtype, {})
        t = turn_by_type.get(qtype, {})
        rows.append(
            "<tr>"
            f"<td><strong>{qtype}</strong></td>"
            f"<td>{render_metric_value(s.get('recall@10'))}</td>"
            f"<td>{render_metric_value(t.get('recall@10'))}</td>"
            f"<td>{render_metric_value(t.get('mrr'))}</td>"
            f"<td>{render_metric_value(t.get('ndcg@10'))}</td>"
            "</tr>"
        )
    return (
        '<h3>Retrieval by Question Type</h3>'
        '<div class="table-wrap"><table><thead><tr><th>Type</th><th class="mono">Session R@10</th>'
        '<th class="mono">Turn R@10</th><th class="mono">Turn MRR</th>'
        f'<th class="mono">Turn NDCG@10</th></tr></thead><tbody>{"".join(rows)}</tbody></table></div>'
    )


def _make_grouped_recall_chart(session_by_type: dict, turn_by_type: dict) -> str:
    labels = sorted(set(session_by_type) | set(turn_by_type))
    session_values = [session_by_type.get(label, {}).get("recall@10", 0.0) for label in labels]
    turn_values = [turn_by_type.get(label, {}).get("recall@10", 0.0) for label in labels]

    if go is None:
        return ""

    fig = go.Figure()
    fig.add_trace(go.Bar(name="Session R@10", x=labels, y=session_values, marker_color="#2563eb"))
    fig.add_trace(go.Bar(name="Turn R@10", x=labels, y=turn_values, marker_color="#16a34a"))
    fig.update_layout(
        barmode="group",
        yaxis=dict(title="Recall@10", range=[0, 1.05], tickformat=".2f"),
        xaxis=dict(title="Question Type", tickangle=-30),
        margin=dict(l=60, r=30, t=20, b=100),
        height=380,
    )
    return '<div class="table-wrap">' + fig.to_html(full_html=False, include_plotlyjs="cdn") + '</div>'


def _render_lme_retrieval_section(metrics: dict, run_meta: dict | None) -> str:
    session_overall = metrics.get("session", {}).get("overall", {})
    turn_overall = metrics.get("turn", {}).get("overall", {})
    session_by_type = metrics.get("session", {}).get("by_type", {})
    turn_by_type = metrics.get("turn", {}).get("by_type", {})

    parts = []
    parts.append(_render_retrieval_overall_table(session_overall, turn_overall))
    chart = _make_grouped_recall_chart(session_by_type, turn_by_type)
    if chart:
        parts.append(f'<div class="chart-wrap">{chart}</div>')
    parts.append(_render_retrieval_by_type_table(session_by_type, turn_by_type))
    return "".join(parts)


# ── LME Failure section ─────────────────────────────────────────────────────

def _render_lme_failure_summary_table(qa_metrics: dict) -> str:
    diagnostics = qa_metrics.get("diagnostics") or {}
    analysis = qa_metrics.get("error_analysis") or {}
    rows = [
        ("wrong_answers", analysis.get("num_wrong", 0), "Total failed QA items"),
        ("wrong_gold_hit", diagnostics.get("wrong_gold_hit", 0), "Wrong despite retrieved gold evidence"),
        ("wrong_gold_miss", diagnostics.get("wrong_gold_miss", 0), "Wrong and missed gold evidence"),
        ("gold_hit_rate", fmt_percent(diagnostics.get("gold_hit_rate")), "Gold evidence in QA top-k"),
    ]
    body = "".join(
        f'<tr><td>{humanize_label(name)}</td><td class="mono">{value}</td><td>{meaning}</td></tr>'
        for name, value, meaning in rows
    )
    return (
        '<div class="table-wrap"><table><thead><tr><th>Signal</th><th class="mono">Value</th>'
        f'<th>Meaning</th></tr></thead><tbody>{body}</tbody></table></div>'
    )


def _render_lme_error_analysis_table(qa_metrics: dict, error_report_href: str | None = None) -> str:
    analysis = qa_metrics.get("error_analysis") or {}
    primary_counts = analysis.get("primary_counts") or {}

    primary_rows = []
    for label, count in sorted(primary_counts.items(), key=lambda item: (-item[1], item[0])):
        primary_rows.append(
            "<tr>"
            f'<td>{humanize_label(label)}</td>'
            f'<td class="mono">{count}</td>'
            f"<td>{_ERROR_LABEL_MEANING.get(label, '')}</td>"
            "</tr>"
        )

    link_html = render_action_link(error_report_href, "View full error report") if error_report_href else ""
    return (
        "<h3>Primary Error Types</h3>"
        '<div class="table-wrap"><table><thead><tr><th>Type</th><th class="mono">Count</th>'
        f'<th>Meaning</th></tr></thead><tbody>{"".join(primary_rows)}</tbody></table></div>'
        + link_html
    )


def _render_lme_failure_section(qa_metrics: dict) -> str:
    return (
        _render_lme_failure_summary_table(qa_metrics)
        + _render_lme_error_analysis_table(qa_metrics)
    )


def _primary_error(item: dict) -> str:
    analysis = item.get("error_analysis") or {}
    return str(analysis.get("primary") or "unknown")


def _render_lme_full_error_report(results: list[dict]) -> str:
    wrong = [item for item in results if item.get("correct") is not True]
    if not wrong:
        return '<div class="note">No QA failures available.</div>'
    grouped: dict[str, list[dict]] = {}
    for item in wrong:
        grouped.setdefault(_primary_error(item), []).append(item)

    parts = []
    for label, items in sorted(grouped.items(), key=lambda pair: (-len(pair[1]), pair[0])):
        rows = []
        for item in items:
            rows.append(
                "<tr>"
                f'<td class="mono">{html_escape(item.get("question_id", ""))}</td>'
                f'<td class="mono">{html_escape(item.get("question_type", ""))}</td>'
                f'{render_text_cell(item.get("question", ""))}'
                f'{render_text_cell(item.get("correct_answer", ""))}'
                f'{render_text_cell(item.get("generated_answer", ""))}'
                "</tr>"
            )
        table = (
            '<div class="table-wrap"><table><thead><tr><th class="mono">Question ID</th>'
            '<th class="mono">Type</th><th>Question</th><th>Gold</th><th>Generated</th>'
            f'</tr></thead><tbody>{"".join(rows)}</tbody></table></div>'
        )
        parts.append(
            f'<details open><summary>{humanize_label(label)} ({len(items)})</summary>'
            f'<div class="details-body">{table}</div></details>'
        )
    return "".join(parts)


def generate_longmemeval_error_report(
    results: list[dict],
    output_path: str,
    *,
    run_meta: dict | None = None,
) -> None:
    from common.report import generate_report

    run_meta = run_meta or {}
    generate_report(
        output_path=output_path,
        title="RAM-A LongMemEval Error Report",
        header_meta={
            "Dataset": "LongMemEval",
            "Backend": _display_backend_name(run_meta.get("backend")),
            "Errors": sum(1 for item in results if item.get("correct") is not True),
        },
        scorecard_html="",
        sections=[
            {
                "title": "Failure Details",
                "subtitle": "Grouped by primary error type.",
                "html": render_action_link("report.html", "Back to main report") + _render_lme_full_error_report(results),
            }
        ],
        run_meta=run_meta,
        show_run_info=False,
    )


# ── Header meta ─────────────────────────────────────────────────────────────

def _build_header_meta(
    run_meta: dict,
    dataset: str,
    embedding_model: str,
    qa_metrics: dict | None,
    metrics: dict,
    git_hash: str,
) -> dict:
    from datetime import datetime, timezone

    embedding_label = " / ".join(
        str(part) for part in [
            run_meta.get("embedding_type"),
            embedding_model,
            f"{run_meta.get('dimensions')}d" if run_meta.get("dimensions") else None,
        ] if part
    ) or embedding_model
    qa_label = " / ".join(
        str(part) for part in [run_meta.get("answerer_model"), run_meta.get("judge_model")] if part
    )
    display_questions = (qa_metrics or {}).get("overall", {}).get("total") or metrics.get("num_questions", 0)

    meta = {
        "Date": datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC"),
        "Backend": _display_backend_name(run_meta.get("backend")),
        "Dataset": dataset,
        "Questions": display_questions,
        "Embedding": embedding_label,
    }
    if qa_label:
        meta["QA"] = qa_label
    meta["Git"] = git_hash
    return meta


# ── Entry point ─────────────────────────────────────────────────────────────

def generate_longmemeval_report(
    metrics: dict,
    output_path: str,
    dataset: str = "longmemeval_oracle",
    embedding_model: str = "baai/bge-m3",
    git_hash: str = "unknown",
    qa_metrics: dict | None = None,
    run_meta: dict | None = None,
    error_report_href: str | None = None,
) -> None:
    """Generate a LongMemEval HTML report."""
    from common.report import generate_report

    run_meta = run_meta or {}

    header_meta = _build_header_meta(run_meta, dataset, embedding_model, qa_metrics, metrics, git_hash)
    scorecard_html = _render_lme_scorecard(metrics, qa_metrics)
    warnings = _build_lme_warnings(metrics)

    sections = []

    sections.append({
        "title": "Benchmark Summary",
        "html": _render_lme_benchmark_summary(qa_metrics),
    })

    if qa_metrics:
        chart_html = make_bar_chart(
            qa_metrics.get("by_type", {}), "accuracy",
            x_title="Question Type", y_title="Accuracy",
            value_format="percent",
        )
        sections.append({
            "title": "QA Accuracy by Question Type",
            "html": f'<div class="chart-wrap">{chart_html}</div>' + _render_lme_qa_by_type_table(qa_metrics),
        })

    sections.append({
        "title": "Retrieval Metrics",
        "subtitle": "These metrics explain memory retrieval quality before QA answering.",
        "html": _render_lme_retrieval_section(metrics, run_meta),
    })

    if qa_metrics and qa_metrics.get("error_analysis", {}).get("num_wrong", 0) > 0:
        sections.append({
            "title": "Failure Analysis",
            "subtitle": "Shows failure counts and primary error types. Use the full error report for item-level review.",
            "html": (
                _render_lme_failure_summary_table(qa_metrics)
                + _render_lme_error_analysis_table(qa_metrics, error_report_href)
            ),
        })

    display_run_meta = dict(run_meta)
    display_run_meta.pop("answer_prompt_version", None)

    generate_report(
        output_path=output_path,
        title="RAM-A LongMemEval Report",
        header_meta=header_meta,
        scorecard_html=scorecard_html,
        sections=sections,
        warnings=warnings or None,
        run_meta=display_run_meta,
        back_to_index_href=relative_href(DASHBOARD_PATH, output_path),
    )

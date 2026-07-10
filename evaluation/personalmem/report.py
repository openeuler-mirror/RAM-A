"""PersonaMem HTML report rendering."""

from __future__ import annotations

from pathlib import Path

from common.report import (
    render_action_link,
    fmt_float,
    fmt_int,
    fmt_percent,
    generate_report,
    html_escape,
    render_card,
    render_metric_value,
    render_text_cell,
    relative_href,
)

PROJECT_ROOT = Path(__file__).resolve().parents[2]
DASHBOARD_PATH = PROJECT_ROOT / "outputs" / "index.html"


def _display_backend_name(value: str | None) -> str:
    return "RAM-A" if value is None else str(value)


def generate_personamem_report(
    report: dict,
    output_path: str,
    *,
    run_meta: dict | None = None,
) -> None:
    """Render PersonaMem retrieval or answer-grade metrics as HTML."""
    report_type = report.get("report_type")
    if report_type == "grade":
        _generate_personamem_grade_report(report, output_path, run_meta=run_meta)
        return
    if report_type not in (None, "retrieval"):
        raise ValueError(f"unsupported PersonaMem report_type: {report_type}")
    _generate_personamem_retrieval_report(report, output_path, run_meta=run_meta)


def generate_personamem_main_report(
    *,
    output_path: str,
    retrieval_report: dict | None = None,
    grade_report: dict | None = None,
    error_report_href: str | None = None,
    run_meta: dict | None = None,
) -> None:
    """Render the single per-run PersonaMem report."""
    run_meta = run_meta or {}
    sections = []
    warnings = []
    scorecard_parts = []

    if retrieval_report:
        query_count = int(retrieval_report.get("query_count") or 0)
        queries_with_gold = int(retrieval_report.get("queries_with_gold") or 0)
        retrieval_supported = retrieval_report.get("retrieval_scoring_supported") is not False
        if not retrieval_supported:
            warnings.append(
                "PersonaMem has no gold evidence IDs, so retrieval metrics are diagnostic only. "
                "Use QA Accuracy as the primary benchmark score."
            )
        elif query_count and queries_with_gold < query_count:
            warnings.append(
                f"{query_count - queries_with_gold} query(s) were not scored because gold evidence was missing."
            )
        if retrieval_supported:
            scorecard_parts.extend([
                render_card("Retrieval Hit@K", fmt_percent(retrieval_report.get("hit_at_k")), f"k={retrieval_report.get('top_k')}", score_value=retrieval_report.get("hit_at_k")),
                render_card("Retrieval MRR", fmt_float(retrieval_report.get("mrr"), 4), "first gold rank", score_value=retrieval_report.get("mrr")),
            ])
        else:
            scorecard_parts.extend([
                render_card("Query Count", fmt_int(retrieval_report.get("query_count")), "retrieval complete"),
                render_card("Retrieved Tokens", fmt_int(retrieval_report.get("avg_context_tokens")), "avg estimated tokens"),
            ])
        sections.append({
            "title": "Retrieval Metrics" if retrieval_supported else "Retrieval Diagnostics",
            "html": _render_summary_table(retrieval_report),
        })

    if grade_report:
        summary = grade_report.get("summary", {})
        total = int(summary.get("total") or 0)
        valid = int(summary.get("valid_predictions") or 0)
        correct = int(summary.get("correct") or 0)
        failures = [item for item in grade_report.get("per_query", []) if not item.get("is_correct")]
        scorecard_parts.extend([
            render_card("QA Accuracy", fmt_percent(summary.get("answer_acc")), f"{correct}/{total} correct", score_value=summary.get("answer_acc")),
            render_card("Answer Tokens", fmt_int(summary.get("avg_context_tokens")), "avg estimated tokens"),
            render_card("Latency", _fmt_ms(summary.get("avg_response_latency_ms")), "avg answer latency"),
        ])
        if valid < total:
            warnings.append(f"{total - valid} question(s) did not produce a parseable option label.")
        sections.append({
            "title": "QA / Grade Metrics",
            "html": _render_grade_summary_table(summary),
        })
        if grade_report.get("by_question_type"):
            sections.append({
                "title": "Accuracy by Question Type",
                "html": _render_grade_group_table(grade_report.get("by_question_type", [])),
            })
        if failures:
            link = render_action_link(error_report_href, "View full error report") if error_report_href else ""
            sections.append({
                "title": "Failure Summary",
                "subtitle": "Shows a compact failure summary. Use the full error report for item-level review.",
                "html": link + _render_grade_failure_table(
                    _prioritize_grade_rows(grade_report.get("per_query", []), limit=3),
                    context_token_budget=summary.get("context_token_budget"),
                ),
            })
    elif retrieval_report:
        sections.append({
            "title": "QA / Grade Status",
            "html": (
                '<div class="note">'
                "Answer generation and grading have not been run for this PersonaMem result yet. "
                "The main PersonaMem benchmark score will appear here after the answer and grade stages complete."
                "</div>"
            ),
        })

    if not sections:
        sections.append({"title": "Benchmark Summary", "html": '<div class="note">No PersonaMem metrics available.</div>'})

    generate_report(
        output_path=output_path,
        title="RAM-A PersonaMem Report",
        header_meta={
            "Dataset": "PersonaMem",
            "Backend": _display_backend_name(run_meta.get("backend")),
            "Questions": (grade_report or {}).get("summary", {}).get("total") or (retrieval_report or {}).get("query_count", "unknown"),
        },
        scorecard_html="".join(scorecard_parts),
        sections=sections,
        warnings=warnings,
        run_meta=_personamem_display_run_meta(run_meta, retrieval_report),
        back_to_index_href=relative_href(DASHBOARD_PATH, output_path),
    )


def generate_personamem_error_report(
    *,
    output_path: str,
    retrieval_report: dict | None = None,
    grade_report: dict | None = None,
    run_meta: dict | None = None,
) -> None:
    sections = []
    back_link = render_action_link("report.html", "Back to main report")
    if retrieval_report:
        if retrieval_report.get("retrieval_scoring_supported") is False:
            sections.append({
                "title": "Retrieval Diagnostics",
                "html": back_link + (
                    f'<div class="note">{html_escape(retrieval_report.get("unsupported_reason", "Retrieval scoring is not available for this dataset."))}</div>'
                ),
            })
        else:
            rows = [item for item in retrieval_report.get("per_query", []) if item.get("has_gold") and not item.get("hit")]
            sections.append({
                "title": "Retrieval Misses",
                "subtitle": "Queries with gold evidence that was not retrieved.",
                "html": back_link + _render_retrieval_per_query_table(rows),
            })
        back_link = ""
    if grade_report:
        failures = [item for item in grade_report.get("per_query", []) if not item.get("is_correct")]
        sections.append({
            "title": "QA / Grade Failures",
            "subtitle": "Wrong, invalid, and errored answers grouped by question type.",
            "html": back_link + _render_grouped_grade_failures(
                failures,
                context_token_budget=(grade_report.get("summary") or {}).get("context_token_budget"),
            ),
        })
    if not sections:
        sections.append({"title": "Failure Details", "html": back_link + '<div class="note">No failures available.</div>'})
    generate_report(
        output_path=output_path,
        title="RAM-A PersonaMem Error Report",
        header_meta={"Dataset": "PersonaMem", "Backend": _display_backend_name((run_meta or {}).get("backend"))},
        scorecard_html="",
        sections=sections,
        run_meta=run_meta or {},
        show_run_info=False,
    )


def _generate_personamem_retrieval_report(
    report: dict,
    output_path: str,
    *,
    run_meta: dict | None = None,
) -> None:
    """Render PersonaMem retrieval metrics as a compact HTML report."""
    run_meta = run_meta or {}
    summary = {
        "Dataset": report.get("dataset", "unknown"),
        "Model": report.get("model", "unknown"),
        "Top K": report.get("top_k", "unknown"),
        "Queries": report.get("query_count", "unknown"),
    }

    warnings = []
    query_count = int(report.get("query_count") or 0)
    queries_with_gold = int(report.get("queries_with_gold") or 0)
    if query_count and queries_with_gold < query_count:
        if report.get("retrieval_scoring_supported") is False:
            warnings.append(report.get("unsupported_reason", "Retrieval scoring is not available for this dataset."))
        else:
            warnings.append(
                f"{query_count - queries_with_gold} query(s) were not scored because gold evidence was missing."
            )

    retrieval_supported = report.get("retrieval_scoring_supported") is not False
    scorecard_items = []
    if retrieval_supported:
        scorecard_items.extend([
            render_card("Accuracy", fmt_percent(report.get("acc")), f"{queries_with_gold} scored queries", score_value=report.get("acc")),
            render_card("Hit@K", fmt_percent(report.get("hit_at_k")), f"k={report.get('top_k')}", score_value=report.get("hit_at_k")),
            render_card("MRR", fmt_float(report.get("mrr"), 4), "mean reciprocal rank", score_value=report.get("mrr")),
        ])
    scorecard_items.append(render_card("Context Tokens", fmt_int(report.get("avg_context_tokens")), "avg estimated tokens"))
    scorecard = "".join(scorecard_items)

    sections = [
        {
            "title": "Benchmark Summary",
            "html": _render_summary_table(report),
        },
        {
            "title": "Per-Query Results",
            "subtitle": "Misses are shown first, followed by the first scored rows.",
            "html": _render_retrieval_per_query_table(_prioritize_retrieval_rows(report.get("per_query", []))),
        },
    ]

    generate_report(
        output_path=output_path,
        title="RAM-A PersonaMem Report",
        header_meta=summary,
        scorecard_html=scorecard,
        sections=sections,
        warnings=warnings,
        run_meta=run_meta,
    )


def _generate_personamem_grade_report(
    report: dict,
    output_path: str,
    *,
    run_meta: dict | None = None,
) -> None:
    """Render PersonaMem answer accuracy, cost, latency, and failures."""
    run_meta = run_meta or {}
    summary = report.get("summary", {})
    total = int(summary.get("total") or 0)
    valid = int(summary.get("valid_predictions") or 0)
    correct = int(summary.get("correct") or 0)
    api_errors = int(summary.get("api_error_count") or 0)
    parse_errors = int(summary.get("parse_error_count") or 0)
    failures = [item for item in report.get("per_query", []) if not item.get("is_correct")]

    header_meta = {
        "Dataset": "PersonaMem",
        "Mode": "answer accuracy",
        "Questions": total,
    }
    warnings = []
    if valid < total:
        warnings.append(f"{total - valid} question(s) did not produce a parseable option label.")
    if api_errors or parse_errors:
        warnings.append(f"api_errors={api_errors}, parse_errors={parse_errors}")

    scorecard = "".join(
        [
            render_card("Accuracy", fmt_percent(summary.get("answer_acc")), f"{correct}/{total} correct", score_value=summary.get("answer_acc")),
            render_card("Valid Accuracy", fmt_percent(summary.get("valid_answer_acc")), f"{valid} valid predictions", score_value=summary.get("valid_answer_acc")),
            render_card("Context Tokens", fmt_int(summary.get("avg_context_tokens")), "avg estimated tokens"),
            render_card("Latency", _fmt_ms(summary.get("avg_response_latency_ms")), "avg answer latency"),
            render_card("Failures", fmt_int(len(failures)), "wrong or invalid answers"),
        ]
    )

    sections = [
        {
            "title": "Benchmark Summary",
            "html": _render_grade_summary_table(summary),
        },
        {
            "title": "Accuracy by Question Type",
            "html": _render_grade_group_table(report.get("by_question_type", [])),
        },
        {
            "title": "Failure Analysis",
            "subtitle": "Wrong, invalid, and errored questions are shown first for manual review.",
            "html": _render_grade_failure_table(
                _prioritize_grade_rows(report.get("per_query", [])),
                context_token_budget=summary.get("context_token_budget"),
            ),
        },
    ]
    generate_report(
        output_path=output_path,
        title="RAM-A PersonaMem Report",
        header_meta=header_meta,
        scorecard_html=scorecard,
        sections=sections,
        warnings=warnings,
        run_meta=run_meta,
    )


def _render_summary_table(report: dict) -> str:
    if report.get("retrieval_scoring_supported") is False:
        rows = [
            ("Top K", fmt_int(report.get("top_k"))),
            ("Query Count", fmt_int(report.get("query_count"))),
            ("Retrieved Context Tokens", fmt_float(report.get("avg_context_tokens"), 2)),
        ]
        note = (
            '<div class="note">'
            "PersonaMem has no gold evidence IDs, so retrieval metrics are diagnostic only. "
            "Use QA Accuracy as the primary benchmark score."
            "</div>"
        )
    else:
        rows = [
            ("Accuracy", render_metric_value(report.get("acc"))),
            ("Hit At K", render_metric_value(report.get("hit_at_k"))),
            ("MRR", render_metric_value(report.get("mrr"))),
            ("Retrieved Context Tokens", fmt_float(report.get("avg_context_tokens"), 2)),
            ("Query Count", fmt_int(report.get("query_count"))),
            ("Queries With Gold", fmt_int(report.get("queries_with_gold"))),
        ]
        note = ""
    body = "".join(
        f'<tr><td>{html_escape(name)}</td><td class="mono">{value}</td></tr>'
        for name, value in rows
    )
    return (
        note + '<div class="table-wrap"><table><thead><tr><th>Metric</th><th class="mono">Value</th></tr></thead>'
        f"<tbody>{body}</tbody></table></div>"
    )


def _render_grade_summary_table(summary: dict) -> str:
    rows = [
        ("QA Accuracy", render_metric_value(summary.get("answer_acc"))),
        ("Correct", f'{fmt_int(summary.get("correct"))} / {fmt_int(summary.get("total"))}'),
        ("Valid Predictions", f'{fmt_int(summary.get("valid_predictions"))} / {fmt_int(summary.get("total"))}'),
        ("Answer Context Tokens", fmt_float(summary.get("avg_context_tokens"), 2)),
        ("Avg Answer Latency", fmt_float(summary.get("avg_response_latency_ms"), 2)),
        ("API Errors", fmt_int(summary.get("api_error_count"))),
        ("Parse Errors", fmt_int(summary.get("parse_error_count"))),
        ("Avg Retrieved Contexts", fmt_float(summary.get("avg_retrieved_contexts"), 2)),
    ]
    body = "".join(
        f'<tr><td>{html_escape(name)}</td><td class="mono">{value}</td></tr>'
        for name, value in rows
    )
    return (
        '<div class="table-wrap"><table><thead><tr><th>Metric</th><th class="mono">Value</th></tr></thead>'
        f"<tbody>{body}</tbody></table></div>"
    )


def _render_grade_group_table(groups: list[dict]) -> str:
    if not groups:
        return '<p class="subtle">No grouped grade metrics available.</p>'
    rows = []
    for group in groups:
        rows.append(
            "<tr>"
            f'{render_text_cell(group.get("name", "unknown"))}'
            f'<td class="mono">{render_metric_value(group.get("accuracy"))}</td>'
            f'<td class="mono">{fmt_int(group.get("correct"))} / {fmt_int(group.get("total"))}</td>'
            f'<td class="mono">{fmt_int(group.get("wrong"))}</td>'
            f'<td class="mono">{fmt_float(group.get("avg_context_tokens"), 2)}</td>'
            f'<td class="mono">{fmt_int(group.get("wrong_near_token_budget"))}</td>'
            "</tr>"
        )
    return (
        '<div class="table-wrap"><table><thead><tr>'
        '<th>Question Type</th><th class="mono">Accuracy</th><th class="mono">Correct</th>'
        '<th class="mono">Wrong</th><th class="mono">Answer Context Tokens</th>'
        '<th class="mono">Wrong Near Budget</th>'
        f"</tr></thead><tbody>{''.join(rows)}</tbody></table></div>"
    )


def _render_retrieval_per_query_table(items: list[dict]) -> str:
    if not items:
        return '<p class="subtle">No per-query rows available.</p>'
    rows = []
    for item in items:
        rows.append(
            "<tr>"
            f'<td class="mono">{html_escape(item.get("query_path", ""))}</td>'
            f'{render_text_cell(item.get("query", ""))}'
            f'<td class="mono">{html_escape(item.get("hit"))}</td>'
            f'<td class="mono">{_fmt_rank(item.get("rank"))}</td>'
            f'{render_text_cell(", ".join(str(v) for v in item.get("gold", [])))}'
            "</tr>"
        )
    return (
        '<div class="table-wrap"><table><thead><tr>'
        '<th class="mono">Query Path</th><th>Query</th><th class="mono">Hit</th>'
        '<th class="mono">Rank</th><th>Gold</th>'
        f"</tr></thead><tbody>{''.join(rows)}</tbody></table></div>"
    )


def _render_grouped_grade_failures(items: list[dict], *, context_token_budget=None) -> str:
    if not items:
        return '<p class="subtle">No failures available.</p>'
    groups: dict[str, list[dict]] = {}
    for item in items:
        key = str(item.get("question_type") or "unknown")
        groups.setdefault(key, []).append(item)
    parts = []
    for key, group_items in sorted(groups.items(), key=lambda pair: (-len(pair[1]), pair[0])):
        parts.append(
            f"<h3>{html_escape(_display_group_name(key))} <span class=\"subtle\">({len(group_items)} wrong)</span></h3>"
            + _render_grade_failure_table(group_items, context_token_budget=context_token_budget)
        )
    return "".join(parts)


def _render_grade_failure_table(items: list[dict], *, context_token_budget=None) -> str:
    if not items:
        return '<p class="subtle">No failures available.</p>'
    show_issue = any(item.get("error") or item.get("parse_error") for item in items)
    show_notes = any(_grade_failure_note(item, context_token_budget) for item in items)
    rows = []
    for item in items:
        issue_cell = ""
        if show_issue:
            issue_cell = render_text_cell(item.get("error", "") or item.get("parse_error", ""))
        note_cell = ""
        if show_notes:
            note_cell = render_text_cell(_grade_failure_note(item, context_token_budget))
        rows.append(
            "<tr>"
            f'<td class="mono">{html_escape(item.get("question_id", ""))}</td>'
            f'<td class="mono">{html_escape(_display_group_name(item.get("question_type") or "unknown"))}</td>'
            f'{render_text_cell(item.get("question", ""))}'
            f'{render_text_cell(_format_option_answer(item.get("predicted_answer"), item.get("predicted_answer_text")))}'
            f'{render_text_cell(_format_option_answer(item.get("correct_answer"), item.get("correct_answer_text")))}'
            f'<td class="mono">{html_escape(item.get("is_correct"))}</td>'
            f'<td class="mono">{html_escape(item.get("estimated_context_tokens"))}</td>'
            f"{note_cell}"
            f"{issue_cell}"
            "</tr>"
        )
    issue_header = "<th>Issue</th>" if show_issue else ""
    note_header = "<th>Notes</th>" if show_notes else ""
    return (
        '<div class="table-wrap"><table><thead><tr>'
        '<th class="mono">Question ID</th><th class="mono">Type</th><th>Question</th><th>Predicted</th>'
        '<th>Gold</th><th class="mono">Correct</th><th class="mono">Ctx Tokens</th>'
        f"{note_header}{issue_header}"
        f"</tr></thead><tbody>{''.join(rows)}</tbody></table></div>"
    )


def _format_option_answer(label, text) -> str:
    label_text = str(label or "").strip()
    full_text = str(text or "").strip()
    if full_text and full_text != label_text:
        return full_text
    return label_text


def _grade_failure_note(item: dict, context_token_budget) -> str:
    try:
        budget = int(context_token_budget or 0)
        tokens = float(item.get("estimated_context_tokens") or 0)
    except (TypeError, ValueError):
        return ""
    if budget > 0 and tokens >= budget * 0.95:
        return "Near token budget"
    return ""


def _display_group_name(value) -> str:
    text = str(value or "unknown")
    if not text:
        return "Unknown"
    return text.replace("_", " ").replace("-", " ").title()


def _prioritize_retrieval_rows(items: list[dict], limit: int = 100) -> list[dict]:
    misses = [item for item in items if item.get("has_gold") and not item.get("hit")]
    unscored = [item for item in items if not item.get("has_gold")]
    others = [item for item in items if item not in misses and item not in unscored]
    return (misses + unscored + others)[:limit]


def _prioritize_grade_rows(items: list[dict], limit: int = 100) -> list[dict]:
    failures = [item for item in items if not item.get("is_correct")]
    successes = [item for item in items if item.get("is_correct")]
    return (failures + successes)[:limit]


def _fmt_ms(value: float | None) -> str:
    if value is None:
        return "n/a"
    return f"{value:.0f} ms"


def _fmt_rank(value) -> str:
    return "n/a" if value is None else html_escape(value)


def _escape(value: object) -> str:
    return html_escape(value)


def _personamem_display_run_meta(run_meta: dict, retrieval_report: dict | None) -> dict:
    meta = dict(run_meta)
    if retrieval_report:
        meta.setdefault("top_k", retrieval_report.get("top_k"))
        meta.setdefault("embedding", retrieval_report.get("embedding"))
        meta.setdefault("embedding_model", retrieval_report.get("model"))
    return meta

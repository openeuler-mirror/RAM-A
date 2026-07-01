import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path
from statistics import mean, median

EVALUATION_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(EVALUATION_ROOT))

from common.report import fmt_float, fmt_int, fmt_percent, generate_report, html_escape, render_card, render_metric_value, render_text_cell

SCORE_FIELDS = ("bleu_score", "f1_score", "llm_score")
TOKEN_FIELDS = ("prompt_tokens", "completion_tokens", "total_tokens")
LATENCY_FIELD = "response_time"


def load_items(input_file):
    with input_file.open("r") as f:
        data = json.load(f)

    return [item for items in data.values() for item in items]


def aggregate_scores(items):
    scored_items = [item for item in items if category_id(item) != 5]
    if not scored_items:
        raise ValueError("Evaluation result file contains no scored items.")

    category_values = defaultdict(lambda: defaultdict(list))
    overall_values = defaultdict(list)

    for item in scored_items:
        category = int(item["category"])
        for score_field in SCORE_FIELDS:
            value = float(item[score_field])
            category_values[category][score_field].append(value)
            overall_values[score_field].append(value)
        for token_field in TOKEN_FIELDS:
            value = item.get(token_field)
            if value is not None:
                category_values[category][token_field].append(float(value))
                overall_values[token_field].append(float(value))
        latency = item.get(LATENCY_FIELD)
        if latency is not None:
            category_values[category][LATENCY_FIELD].append(float(latency))
            overall_values[LATENCY_FIELD].append(float(latency))

    category_scores = {}
    for category, values in sorted(category_values.items()):
        category_scores[category] = {
            score_field: round(mean(values[score_field]), 4) for score_field in SCORE_FIELDS
        }
        for token_field in TOKEN_FIELDS:
            token_values = values[token_field]
            category_scores[category][f"avg_{token_field}"] = round(mean(token_values), 2) if token_values else None
        latency_values = values[LATENCY_FIELD]
        category_scores[category]["latency_p50_seconds"] = round(median(latency_values), 2) if latency_values else None
        category_scores[category]["latency_p95_seconds"] = round(percentile(latency_values, 95), 2) if latency_values else None
        category_scores[category]["count"] = len(values[SCORE_FIELDS[0]])

    overall_scores = {score_field: round(mean(overall_values[score_field]), 4) for score_field in SCORE_FIELDS}
    for token_field in TOKEN_FIELDS:
        token_values = overall_values[token_field]
        overall_scores[f"avg_{token_field}"] = round(mean(token_values), 2) if token_values else None
    latency_values = overall_values[LATENCY_FIELD]
    overall_scores["latency_p50_seconds"] = round(median(latency_values), 2) if latency_values else None
    overall_scores["latency_p95_seconds"] = round(percentile(latency_values, 95), 2) if latency_values else None
    overall_scores["count"] = len(scored_items)
    overall_scores["skipped_count"] = len(items) - len(scored_items)
    return category_scores, overall_scores


def format_optional(value, digits=2):
    if value is None:
        return "n/a"
    return f"{value:.{digits}f}"


def print_scores(category_scores, overall_scores):
    print("Mean Scores Per Category:")
    print(
        f"{'category':>8} {'bleu_score':>12} {'f1_score':>10} {'llm_score':>11} "
        f"{'avg_prompt_tokens':>18} {'avg_completion_tokens':>23} "
        f"{'avg_total_tokens':>18} {'latency_p50_seconds':>21} {'count':>7}"
    )
    for category, scores in category_scores.items():
        print(
            f"{category:>8} {scores['bleu_score']:>12.4f} {scores['f1_score']:>10.4f} "
            f"{scores['llm_score']:>11.4f} "
            f"{format_optional(scores['avg_prompt_tokens']):>18} "
            f"{format_optional(scores['avg_completion_tokens']):>23} "
            f"{format_optional(scores['avg_total_tokens']):>18} "
            f"{format_optional(scores['latency_p50_seconds']):>21} "
            f"{scores['count']:>7}"
        )

    print("\nOverall Mean Scores:")
    for score_field in SCORE_FIELDS:
        print(f"{score_field:<12} {overall_scores[score_field]:.4f}")
    for token_field in TOKEN_FIELDS:
        print(f"avg_{token_field:<25} {format_optional(overall_scores[f'avg_{token_field}'])}")
    print(f"latency_p50_seconds{'':<13} {format_optional(overall_scores['latency_p50_seconds'])}")
    print(f"latency_p95_seconds{'':<13} {format_optional(overall_scores['latency_p95_seconds'])}")


def build_report(input_file, category_scores, overall_scores, items):
    return {
        "dataset": "locomo",
        "input": str(input_file),
        "overall": overall_scores,
        "by_category": {str(key): value for key, value in category_scores.items()},
        "failures": build_failure_rows(items),
    }


def write_json_report(path, report):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")


def write_html_report(path, report):
    overall = report.get("overall", {})
    by_category = report.get("by_category", {})
    failures = report.get("failures", [])
    scorecard = "".join(
        [
            render_card("LLM Score", fmt_float(overall.get("llm_score"), 4), "mean judge score", score_value=overall.get("llm_score"), metric_type="binary"),
            render_card("F1", fmt_float(overall.get("f1_score"), 4), "mean token overlap", score_value=overall.get("f1_score")),
            render_card("Answer Tokens", fmt_int(overall.get("avg_total_tokens")), "avg total tokens"),
            render_card("Latency P50/P95", _latency_pair(overall), "seconds"),
            render_card("Failures", fmt_int(len(failures)), "LLM judge score < 1"),
        ]
    )
    sections = [
        {"title": "Benchmark Summary", "html": render_overall_table(overall)},
        {"title": "Scores by Category", "html": render_category_table(by_category)},
        {
            "title": "Failure Analysis",
            "subtitle": "Questions with LLM judge score below 1 are shown first for manual review.",
            "html": render_failure_table(failures),
        },
    ]
    warnings = []
    if overall.get("skipped_count"):
        warnings.append(
            f"{overall['skipped_count']} category-5 adversarial/unanswerable question(s) "
            "were excluded from the main QA score. This follows memory-system "
            "baseline practice; evaluate them separately with an abstention rubric."
        )
    generate_report(
        output_path=str(path),
        title="RAM-A LoCoMo Report",
        header_meta={
            "Dataset": "LoCoMo",
            "Input": report.get("input", "unknown"),
            "Categories": len(by_category),
        },
        scorecard_html=scorecard,
        sections=sections,
        warnings=warnings,
        run_meta={"input": report.get("input")},
    )


def render_overall_table(overall):
    rows = []
    labels = {
        "llm_score": "LLM Score",
        "f1_score": "F1",
        "bleu_score": "BLEU-1",
        "avg_prompt_tokens": "Avg Prompt Tokens",
        "avg_completion_tokens": "Avg Completion Tokens",
        "avg_total_tokens": "Answer Total Tokens",
        "latency_p50_seconds": "Latency P50 Seconds",
        "latency_p95_seconds": "Latency P95 Seconds",
        "count": "Count",
        "skipped_count": "Skipped Count",
    }
    for key in labels:
        value = overall.get(key)
        if key in SCORE_FIELDS:
            rendered = render_metric_value(value, metric_type="binary" if key == "llm_score" else "quality")
        elif key in {"count", "skipped_count"}:
            rendered = fmt_int(value)
        else:
            rendered = fmt_float(value, 2)
        rows.append(f'<tr><td>{html_escape(labels[key])}</td><td class="mono">{rendered}</td></tr>')
    return (
        '<div class="table-wrap"><table><thead><tr><th>Metric</th><th class="mono">Value</th></tr></thead>'
        f"<tbody>{''.join(rows)}</tbody></table></div>"
    )


def render_category_table(by_category):
    if not by_category:
        return '<p class="subtle">No category rows available.</p>'
    rows = []
    for category, scores in sorted(by_category.items(), key=lambda item: int(item[0])):
        rows.append(
            "<tr>"
            f'<td class="mono">{html_escape(category)}</td>'
            f'<td class="mono">{render_metric_value(scores.get("llm_score"), metric_type="binary")}</td>'
            f'<td class="mono">{render_metric_value(scores.get("f1_score"))}</td>'
            f'<td class="mono">{render_metric_value(scores.get("bleu_score"))}</td>'
            f'<td class="mono">{fmt_float(scores.get("avg_total_tokens"), 2)}</td>'
            f'<td class="mono">{fmt_float(scores.get("latency_p50_seconds"), 2)}</td>'
            f'<td class="mono">{fmt_int(scores.get("count"))}</td>'
            "</tr>"
        )
    return (
        '<div class="table-wrap"><table><thead><tr>'
        '<th class="mono">Category</th><th class="mono">LLM Score</th>'
        '<th class="mono">F1</th><th class="mono">BLEU-1</th>'
        '<th class="mono">Answer Tokens</th><th class="mono">Latency P50</th>'
        '<th class="mono">Count</th>'
        f"</tr></thead><tbody>{''.join(rows)}</tbody></table></div>"
    )


def build_failure_rows(items, limit=100):
    failures = []
    for item in items:
        if category_id(item) == 5:
            continue
        try:
            llm_score = float(item.get("llm_score", 0.0))
        except (TypeError, ValueError):
            llm_score = 0.0
        if llm_score >= 1.0:
            continue
        failures.append(
            {
                "category": item.get("category"),
                "question": item.get("question"),
                "gold_answer": item.get("answer"),
                "response": item.get("response"),
                "llm_score": llm_score,
                "f1_score": item.get("f1_score"),
                "bleu_score": item.get("bleu_score"),
                "total_tokens": item.get("total_tokens"),
                "response_time": item.get("response_time"),
            }
        )
    failures.sort(key=lambda row: (float(row.get("llm_score") or 0.0), float(row.get("f1_score") or 0.0)))
    return failures[:limit]


def render_failure_table(items):
    if not items:
        return '<p class="subtle">No failed rows available.</p>'
    show_retrieval = any(
        "retrieval_evidence_hit" in item or "retrieval_first_hit_rank" in item
        for item in items
    )
    rows = []
    for item in items:
        retrieval_cells = ""
        if show_retrieval:
            retrieval_cells = (
                f'<td class="mono">{fmt_percent(item.get("retrieval_evidence_hit"))}</td>'
                f'<td class="mono">{html_escape(item.get("retrieval_first_hit_rank"))}</td>'
            )
        rows.append(
            "<tr>"
            f'<td class="mono">{html_escape(item.get("category"))}</td>'
            f'{render_text_cell(item.get("question", ""))}'
            f'{render_text_cell(item.get("gold_answer", ""))}'
            f'{render_text_cell(item.get("response", ""))}'
            f'<td class="mono">{fmt_float(item.get("llm_score"), 4)}</td>'
            f'<td class="mono">{fmt_float(item.get("f1_score"), 4)}</td>'
            f'<td class="mono">{fmt_float(item.get("total_tokens"), 2)}</td>'
            f"{retrieval_cells}"
            "</tr>"
        )
    retrieval_headers = (
        '<th class="mono">Evidence Hit</th><th class="mono">First Rank</th>'
        if show_retrieval
        else ""
    )
    return (
        '<div class="table-wrap"><table><thead><tr>'
        '<th class="mono">Category</th><th>Question</th><th>Gold</th><th>Response</th>'
        '<th class="mono">LLM</th><th class="mono">F1</th><th class="mono">Answer Tokens</th>'
        f"{retrieval_headers}"
        f"</tr></thead><tbody>{''.join(rows)}</tbody></table></div>"
    )


def category_id(item):
    try:
        return int(item.get("category"))
    except (TypeError, ValueError):
        return -1


def percentile(values, pct):
    values = sorted(float(value) for value in values)
    if not values:
        return None
    if len(values) == 1:
        return values[0]
    rank = (len(values) - 1) * (pct / 100.0)
    low = int(rank)
    high = min(low + 1, len(values) - 1)
    weight = rank - low
    return values[low] * (1 - weight) + values[high] * weight


def _latency_pair(overall):
    p50 = overall.get("latency_p50_seconds")
    p95 = overall.get("latency_p95_seconds")
    if p50 is None and p95 is None:
        return "n/a"
    return f"{fmt_float(p50, 2)} / {fmt_float(p95, 2)}"


def main():
    parser = argparse.ArgumentParser(description="Aggregate evaluation scores by category.")
    parser.add_argument(
        "--input",
        type=Path,
        required=True,
        help="Path to the JSON output generated by evals.py.",
    )
    parser.add_argument("--output-json", type=Path, help="Optional path to save aggregate metrics JSON.")
    parser.add_argument("--html-report", type=Path, help="Optional path to save aggregate metrics HTML.")
    args = parser.parse_args()

    items = load_items(args.input)
    category_scores, overall_scores = aggregate_scores(items)
    print_scores(category_scores, overall_scores)
    report = build_report(args.input, category_scores, overall_scores, items)
    if args.output_json:
        write_json_report(args.output_json, report)
        print(f"Metrics JSON saved to {args.output_json}")
    if args.html_report:
        write_html_report(args.html_report, report)
        print(f"HTML report saved to {args.html_report}")


if __name__ == "__main__":
    main()

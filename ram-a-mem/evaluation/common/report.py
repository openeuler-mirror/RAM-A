"""Generic HTML evaluation report framework.

Provides CSS layout, reusable helpers, and a pluggable section-based template.
Dataset-specific logic lives in each dataset's own report module.
"""

import os
from pathlib import Path

from jinja2 import Template
from markupsafe import Markup, escape

try:
    import plotly.graph_objects as go
except ModuleNotFoundError:
    go = None

_HTML_TEMPLATE = """\
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{{ title }}</title>
<style>
  *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
  body {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
    background: #f5f7fb;
    color: #263238;
    line-height: 1.55;
  }
  .header {
    background: #152238;
    color: #fff;
    padding: 1.5rem;
  }
  .header-inner {
    max-width: 1180px;
    margin: 0 auto;
  }
  .header h1 {
    font-size: 1.45rem;
    font-weight: 650;
    margin-bottom: 0.6rem;
  }
  .header .meta {
    font-size: 0.84rem;
    color: #c5d0df;
    display: flex;
    flex-wrap: wrap;
    gap: 0.9rem 1.2rem;
  }
  .header .meta span { white-space: nowrap; }
  .container {
    max-width: 1180px;
    margin: 0 auto;
    padding: 1.25rem;
  }
  section {
    background: #fff;
    border: 1px solid #e3e8ef;
    border-radius: 8px;
    box-shadow: 0 1px 2px rgba(15, 23, 42, 0.05);
    margin-bottom: 1rem;
    padding: 1rem 1.1rem;
  }
  h2 {
    font-size: 1.15rem;
    font-weight: 650;
    margin-bottom: 0.75rem;
    color: #152238;
  }
  h3 {
    font-size: 0.92rem;
    font-weight: 650;
    margin: 0.8rem 0 0.45rem;
    color: #334155;
  }
  .subtle {
    color: #64748b;
    font-size: 0.84rem;
    margin-top: -0.3rem;
    margin-bottom: 0.75rem;
  }
  .scorecard {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
    gap: 0.8rem;
    margin-bottom: 1rem;
  }
  .card {
    background: #fff;
    border: 1px solid #dbe3ee;
    border-radius: 8px;
    padding: 0.9rem;
    min-height: 88px;
    box-shadow: 0 1px 2px rgba(15, 23, 42, 0.05);
  }
  .card .label {
    color: #64748b;
    font-size: 0.76rem;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.02em;
    margin-bottom: 0.3rem;
  }
  .card .value {
    font-size: 1.55rem;
    font-weight: 700;
    color: #0f172a;
    line-height: 1.15;
  }
  .card .detail {
    color: #64748b;
    font-size: 0.82rem;
    margin-top: 0.35rem;
  }
  .warning {
    background: #fff7ed;
    border: 1px solid #fed7aa;
    border-left: 3px solid #f97316;
    color: #9a3412;
    border-radius: 6px;
    padding: 0.65rem 0.8rem;
    margin-bottom: 1rem;
    font-size: 0.86rem;
  }
  .table-wrap {
    overflow-x: auto;
    -webkit-overflow-scrolling: touch;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.86rem;
  }
  tbody tr:hover { background: #f1f5f9; }
  th, td {
    padding: 0.48rem 0.65rem;
    text-align: left;
    border-bottom: 1px solid #e5eaf0;
    border-right: 1px solid #f1f5f9;
    vertical-align: top;
  }
  th:last-child, td:last-child { border-right: none; }
  th {
    font-weight: 650;
    color: #475569;
    white-space: nowrap;
    background: #f8fafc;
  }
  .comparison-table {
    table-layout: fixed;
  }
  .comparison-table th.method-col,
  .comparison-table td.method-col {
    text-align: right;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .comparison-table .action-link {
    margin-top: 0;
    margin-bottom: 0;
  }
  .comparison-table th.reports-col,
  .comparison-table td.reports-col {
    text-align: right;
  }
  td.mono, th.mono {
    font-family: "SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace;
    font-size: 0.82rem;
  }
  .text-cell {
    max-width: 320px;
    word-break: break-word;
    overflow-wrap: break-word;
  }
  .text-scroll {
    max-height: 7.5rem;
    overflow: auto;
    padding-right: 0.15rem;
  }
  .score-good, .card .value.score-good { color: #15803d; font-weight: 650; }
  .score-bad, .card .value.score-bad { color: #b91c1c; font-weight: 650; }
  .score-watch, .card .value.score-watch { color: #b45309; font-weight: 650; }
  .chart-wrap { margin-top: 0.6rem; }
  .chart-fallback {
    min-width: 600px;
  }
  .bar-row {
    display: grid;
    grid-template-columns: minmax(150px, 220px) minmax(120px, 1fr) 64px;
    gap: 10px;
    align-items: center;
    margin: 8px 0;
  }
  .bar-label {
    overflow-wrap: anywhere;
    white-space: normal;
  }
  .bar-track {
    height: 18px;
    background: #e2e8f0;
    border-radius: 4px;
    overflow: hidden;
  }
  .bar-fill {
    height: 100%;
    background: #2563eb;
  }
  .grid-2 {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 1rem;
  }
  .note {
    background: #f8fafc;
    border: 1px solid #e2e8f0;
    border-radius: 6px;
    color: #475569;
    font-size: 0.84rem;
    padding: 0.65rem 0.8rem;
    margin-bottom: 0.75rem;
  }
  .subsection {
    border: 1px solid #e5eaf0;
    border-radius: 6px;
    padding: 0.85rem;
    margin: 0.85rem 0;
    background: #fcfdff;
  }
  .subsection:first-child { margin-top: 0; }
  .subsection:last-child { margin-bottom: 0; }
  .subsection h3 {
    margin-top: 0;
    padding-bottom: 0.45rem;
    border-bottom: 1px solid #edf2f7;
  }
  .action-link {
    display: inline-block;
    background: #152238;
    color: #fff;
    border-radius: 6px;
    padding: 0.45rem 0.7rem;
    font-size: 0.84rem;
    font-weight: 650;
    text-decoration: none;
    margin-top: 0.75rem;
    margin-bottom: 0.8rem;
  }
  .action-link:hover { background: #263b5f; }
  details {
    border: 1px solid #e2e8f0;
    border-radius: 6px;
    margin-bottom: 0.7rem;
    background: #fff;
  }
  summary {
    cursor: pointer;
    padding: 0.65rem 0.8rem;
    font-weight: 650;
    color: #152238;
    background: #f8fafc;
  }
  details .details-body {
    padding: 0.7rem 0.8rem;
  }
  .run-info {
    background: #f8fafc;
    border: 1px dashed #cbd5e1;
    border-radius: 8px;
    padding: 1rem 1.1rem;
    margin-bottom: 1rem;
  }
  @media (max-width: 980px) {
    .grid-2 { grid-template-columns: 1fr; }
  }
  @media (max-width: 620px) {
    .container { padding: 0.8rem; }
    .card .value { font-size: 1.25rem; }
    .card .label {
      font-size: 0.7rem;
      text-transform: none;
    }
    table { font-size: 0.78rem; }
    th, td { padding: 0.36rem 0.45rem; }
  }
</style>
</head>
<body>
<div class="header">
  <div class="header-inner">
    <h1>{{ title }}</h1>
    <div class="meta">
      {% for key, val in header_meta.items() %}
      <span>{{ key }}: {{ val }}</span>
      {% endfor %}
    </div>
  </div>
</div>

<div class="container">
  {% if back_to_index_href %}
  <a class="action-link back-link" href="{{ back_to_index_href }}">&larr; Back to Dashboard</a>
  {% endif %}
  {% if warnings %}
  <div class="warning">{{ warnings | join(" &middot; ") }}</div>
  {% endif %}

  {% if scorecard_html %}
  <div class="scorecard">
    {{ scorecard_html }}
  </div>
  {% endif %}

  {% for section in sections %}
  <section>
    <h2>{{ section.title }}</h2>
    {% if section.get("subtitle") %}
    <p class="subtle">{{ section.subtitle }}</p>
    {% endif %}
    {{ section.html }}
  </section>
  {% endfor %}

  {% if run_info_html %}
  <div class="run-info">
    <h2>Run Info</h2>
    {{ run_info_html }}
  </div>
  {% endif %}
</div>
</body>
</html>
"""

# ── Format helpers ──────────────────────────────────────────────────────────

def score_class(value: float, *, metric_type: str = "quality") -> str:
    """Highlight special values by metric direction.

    quality: higher is better on a 0-1 scale, e.g. accuracy/recall/F1.
    binary: exact 0/1 correctness labels.
    cost: lower is better on a normalized 0-1 scale, e.g. relative tokens/latency.
    """
    if metric_type == "binary":
        return "score-good" if value >= 1.0 else "score-bad"
    if metric_type == "cost":
        if value <= 0.4:
            return "score-good"
        if value >= 0.7:
            return "score-watch"
        return ""
    if value >= 0.9:
        return "score-good"
    if value < 0.5:
        return "score-bad"
    return ""


def fmt_percent(value: float | None) -> str:
    if value is None:
        return "n/a"
    return f"{value * 100:.2f}%"


def fmt_float(value: float | None, digits: int = 2) -> str:
    if value is None:
        return "n/a"
    return f"{value:.{digits}f}"


def fmt_int(value) -> str:
    if value is None:
        return "n/a"
    return f"{int(value):,}"


def fmt_ms(value: float | None) -> str:
    if value is None:
        return "n/a"
    return f"{value:.2f} ms"


def html_escape(value) -> str:
    """Escape user/data-provided text before injecting into report HTML."""
    return str(escape("" if value is None else value))


def humanize_label(value) -> str:
    """Convert internal snake/kebab-case labels into readable report labels."""
    text = str("" if value is None else value).replace("_", " ").replace("-", " ")
    return " ".join(word.capitalize() for word in text.split())


# ── Generic renderers ───────────────────────────────────────────────────────

def render_metric_value(value: float | None, *, metric_type: str = "quality") -> str:
    if value is None:
        return '<span class="mono">n/a</span>'
    score = score_class(value, metric_type=metric_type)
    class_attr = f"mono {score}" if score else "mono"
    return f'<span class="{class_attr}">{value:.4f}</span>'


def render_text_cell(value) -> str:
    """Render a bounded long-text table cell."""
    return f'<td class="text-cell"><div class="text-scroll">{html_escape(value)}</div></td>'


def render_action_link(href: str, label: str) -> str:
    return f'<a class="action-link" href="{html_escape(href)}">{html_escape(label)}</a>'


def relative_href(target_path: str | os.PathLike, output_path: str | os.PathLike) -> str:
    """Return a POSIX relative link from the output HTML file to another file."""
    target = Path(target_path).resolve()
    output_dir = Path(output_path).resolve().parent
    return Path(os.path.relpath(target, output_dir)).as_posix()


def render_card(
    label: str,
    value: str,
    detail: str = "",
    score_value: float | None = None,
    metric_type: str = "quality",
) -> str:
    value_class = "value"
    if score_value is not None:
        score = score_class(score_value, metric_type=metric_type)
        if score:
            value_class += f" {score}"
    return (
        '<div class="card">'
        f'<div class="label">{html_escape(label)}</div>'
        f'<div class="{value_class}">{html_escape(value)}</div>'
        f'<div class="detail">{html_escape(detail)}</div>'
        '</div>'
    )


def make_bar_chart(
    rows: dict,
    metric_key: str,
    *,
    x_title: str = "Category",
    y_title: str | None = None,
    value_format: str = "raw",
) -> str:
    labels = sorted(rows.keys())
    values = [rows[lab].get(metric_key, 0.0) for lab in labels]
    y_label = y_title or metric_key

    def _fmt_val(v: float) -> str:
        if value_format == "percent":
            return f"{v * 100:.1f}%"
        return f"{v:.3f}"

    if go is None:
        parts = []
        for label, value in zip(labels, values):
            width = max(0.0, min(value, 1.0)) * 100
            parts.append(
                '<div class="bar-row">'
                f'<div class="mono bar-label">{html_escape(label)}</div>'
                '<div class="bar-track">'
                f'<div class="bar-fill" style="width:{width:.1f}%"></div>'
                '</div>'
                f'<div class="mono">{_fmt_val(value)}</div>'
                '</div>'
            )
        return '<div class="table-wrap"><div class="chart-fallback">' + "".join(parts) + "</div></div>"

    fig = go.Figure()
    fig.add_trace(go.Bar(
        x=labels,
        y=values,
        marker_color="#2563eb",
        text=[_fmt_val(v) for v in values],
        textposition="outside",
    ))
    fig.update_layout(
        yaxis=dict(title=y_label, range=[0, 1.05], tickformat=".2f"),
        xaxis=dict(title=x_title, tickangle=-30),
        margin=dict(l=60, r=30, t=20, b=100),
        height=360,
    )
    return '<div class="table-wrap">' + fig.to_html(full_html=False, include_plotlyjs="cdn") + '</div>'


def _render_run_info_table(run_meta: dict) -> str:
    rows = []
    for key in sorted(run_meta):
        value = run_meta.get(key)
        if value is None:
            continue
        rows.append(
            f'<tr><td class="mono">{html_escape(key)}</td><td class="mono">{html_escape(value)}</td></tr>'
        )
    return (
        '<div class="table-wrap"><table><thead><tr><th class="mono">Field</th><th class="mono">Value</th></tr></thead>'
        f'<tbody>{"".join(rows)}</tbody></table></div>'
    )


# ── Entry point ─────────────────────────────────────────────────────────────

def generate_report(
    *,
    output_path: str,
    title: str,
    header_meta: dict,
    scorecard_html: str = "",
    sections: list[dict],
    warnings: list[str] | None = None,
    run_meta: dict | None = None,
    show_run_info: bool = True,
    back_to_index_href: str | None = None,
) -> None:
    """Generate an HTML evaluation report from pre-rendered sections."""
    run_meta = run_meta or {}
    safe_sections = []
    for section in sections:
        safe_sections.append(
            {
                "title": section.get("title", ""),
                "subtitle": section.get("subtitle", "") if section.get("subtitle") else "",
                "html": Markup(section.get("html", "")),
            }
        )

    template = Template(_HTML_TEMPLATE, autoescape=True)
    html = template.render(
        title=title,
        header_meta=header_meta,
        scorecard_html=Markup(scorecard_html),
        sections=safe_sections,
        warnings=warnings or [],
        run_info_html=Markup(_render_run_info_table(run_meta)) if show_run_info else "",
        back_to_index_href=back_to_index_href or "",
    )

    os.makedirs(os.path.dirname(output_path) or ".", exist_ok=True)
    with open(output_path, "w", encoding="utf-8") as f:
        f.write(html)

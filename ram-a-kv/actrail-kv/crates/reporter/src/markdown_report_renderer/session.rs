//! 本文件渲染会话上下文刷新、逐请求前缀复用度与事件证据。

use std::{collections::BTreeSet, fmt::Write};

use actrail_kv_artifacts::{SessionEventType, SessionReport, SessionSwitchEvent};
use anyhow::Result;

use crate::locale::{namespace, LocaleModule, LocaleSnapshot};

use super::format::{code, short_id, table_code};

pub(super) fn render(
    output: &mut String,
    reports: &[SessionReport],
    locale: LocaleSnapshot,
) -> Result<()> {
    let l = locale.create(namespace::REPORT_SESSIONS)?;
    writeln!(output, "## {}\n", l.get("section_title")?)?;
    writeln!(output, "{}\n", l.get("intro")?)?;

    let mut requests = 0usize;
    let mut lcp_bytes = 0usize;
    let mut next_bytes = 0usize;
    let mut refresh_count = 0usize;
    let mut refresh_sessions = 0usize;
    let mut abandoned_bytes = 0usize;
    for report in reports {
        requests += report.request_count;
        let refreshes = effective_refresh_count(report);
        refresh_count += refreshes;
        refresh_sessions += usize::from(refreshes > 0);
        abandoned_bytes += effective_abandoned_prefix_bytes(report);
        for event in &report.events {
            lcp_bytes += event.lcp_bytes;
            next_bytes += event.next_total_bytes;
        }
    }
    writeln!(
        output,
        "| {} | {} |\n|---|---:|",
        l.get("metric_header")?,
        l.get("value_header")?
    )?;
    writeln!(output, "| {} | {} |", l.get("sessions")?, reports.len())?;
    writeln!(output, "| {} | {requests} |", l.get("requests")?)?;
    writeln!(
        output,
        "| {} | {refresh_count} |",
        l.get("refresh_warnings")?
    )?;
    writeln!(
        output,
        "| {} | {refresh_sessions} |",
        l.get("refresh_sessions")?
    )?;
    writeln!(
        output,
        "| {} | {:.1}% |",
        l.get("current_ratio")?,
        ratio(lcp_bytes, next_bytes) * 100.0
    )?;
    writeln!(
        output,
        "| {} | {abandoned_bytes} B |\n",
        l.get("abandoned")?
    )?;

    writeln!(output, "> {}\n", l.get("note")?)?;

    let common_prefix = common_key_prefix(reports.iter().map(|report| report.session_key.as_str()));
    let endpoints: BTreeSet<&str> = reports
        .iter()
        .map(|report| report.endpoint_key.as_str())
        .collect();
    let models: BTreeSet<&str> = reports.iter().map(|report| report.model.as_str()).collect();
    writeln!(
        output,
        "{}: {}. {}: {}.\n",
        l.get("endpoints")?,
        endpoints
            .into_iter()
            .map(code)
            .collect::<Vec<_>>()
            .join(" / "),
        l.get("models")?,
        models.into_iter().map(code).collect::<Vec<_>>().join(" / ")
    )?;
    if !common_prefix.is_empty() {
        writeln!(
            output,
            "{}\n",
            l.format("common_prefix_omitted", &[("prefix", code(&common_prefix))])?
        )?;
    }

    let mut attention: Vec<&SessionReport> = reports
        .iter()
        .filter(|report| effective_refresh_count(report) > 0)
        .collect();
    let mut normal: Vec<&SessionReport> = reports
        .iter()
        .filter(|report| effective_refresh_count(report) == 0)
        .collect();
    attention.sort_by(|left, right| {
        effective_refresh_count(right)
            .cmp(&effective_refresh_count(left))
            .then_with(|| {
                effective_abandoned_prefix_bytes(right).cmp(&effective_abandoned_prefix_bytes(left))
            })
            .then_with(|| left.session_key.cmp(&right.session_key))
    });
    normal.sort_by(|left, right| left.session_key.cmp(&right.session_key));

    if attention.is_empty() {
        writeln!(output, "{}\n", l.get("no_refresh")?)?;
    } else {
        writeln!(
            output,
            "### {}\n",
            l.format(
                "warning_heading",
                &[("sessions", attention.len().to_string())]
            )?
        )?;
        render_session_table(output, &attention, &common_prefix, l)?;
    }
    if !normal.is_empty() {
        writeln!(
            output,
            "### {}\n",
            l.format("detail_heading", &[("sessions", normal.len().to_string())])?
        )?;
        render_session_table(output, &normal, &common_prefix, l)?;
    }

    for report in reports {
        render_events(output, report, &common_prefix, l)?;
    }
    Ok(())
}

fn render_session_table(
    output: &mut String,
    reports: &[&SessionReport],
    common_prefix: &str,
    locale: LocaleModule,
) -> Result<()> {
    writeln!(
        output,
        "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
        locale.get("table_session")?,
        locale.get("table_requests")?,
        locale.get("table_refreshes")?,
        locale.get("table_first_refresh")?,
        locale.get("table_events")?,
        locale.get("table_min_reuse")?,
        locale.get("table_abandoned")?,
        locale.get("table_current_ratio")?,
        locale.get("table_trend")?
    )?;
    output.push_str("|---|---:|---:|---|---|---:|---:|---:|---|\n");
    for report in reports {
        let lcp: usize = report.events.iter().map(|event| event.lcp_bytes).sum();
        let total: usize = report
            .events
            .iter()
            .map(|event| event.next_total_bytes)
            .sum();
        let first_refresh = first_refresh_pair(report)
            .map(|(previous, next)| format!("{previous} → {next}"))
            .unwrap_or_else(|| "—".to_string());
        let minimum = report
            .events
            .iter()
            .filter_map(|event| event.prefix_reuse_ratio)
            .reduce(f64::min)
            .map(|value| format!("{:.0}%", value * 100.0))
            .unwrap_or_else(|| "—".to_string());
        let trend = report
            .events
            .iter()
            .map(|event| {
                let value = event
                    .prefix_reuse_ratio
                    .unwrap_or_else(|| ratio(event.lcp_bytes, event.next_total_bytes));
                format!("{:.0}%", value * 100.0)
            })
            .collect::<Vec<_>>()
            .join(" → ");
        writeln!(
            output,
            "| {} | {} | {} | {} | {} | {} | {} B | {:.0}% | {} |",
            table_code(&session_label(&report.session_key, common_prefix)),
            report.request_count,
            effective_refresh_count(report),
            first_refresh,
            event_tags(report, locale)?,
            minimum,
            effective_abandoned_prefix_bytes(report),
            ratio(lcp, total) * 100.0,
            if trend.is_empty() { "—" } else { &trend }
        )?;
    }
    output.push('\n');
    Ok(())
}

fn render_events(
    output: &mut String,
    report: &SessionReport,
    common_prefix: &str,
    locale: LocaleModule,
) -> Result<()> {
    if report.events.is_empty() {
        return Ok(());
    }
    writeln!(
        output,
        "#### {}\n",
        locale.format(
            "event_title",
            &[(
                "session",
                code(&session_label(&report.session_key, common_prefix)),
            )]
        )?
    )?;
    writeln!(
        output,
        "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
        locale.get("event_round")?,
        locale.get("event_type")?,
        locale.get("event_risk")?,
        locale.get("event_previous")?,
        locale.get("event_next")?,
        locale.get("event_structural_prefix")?,
        locale.get("event_retained_prefix")?,
        locale.get("event_reuse_ratio")?,
        locale.get("event_abandoned")?,
        locale.get("event_recomputed")?,
        locale.get("event_recoverable")?,
        locale.get("event_domain")?,
        locale.get("event_blocks")?
    )?;
    output.push_str("|---|---|---|---|---|---:|---:|---:|---:|---:|---:|---|---|\n");
    for (index, event) in report.events.iter().enumerate() {
        let (previous, next) = event_sequences(event, index);
        let reuse = event
            .prefix_reuse_ratio
            .map(|value| format!("{:.0}%", value * 100.0))
            .unwrap_or_else(|| "—".to_string());
        writeln!(
            output,
            "| {previous} → {next} | {} | {} | {} | {} | {} units / {} B | {} B | {} | {} B | {} B | {} B | {} | {} |",
            event_label(&event.event_type, locale)?,
            if event_is_refresh(event) {
                locale.get("event_refresh")?
            } else {
                "—"
            },
            table_code(&short_id(&event.prev_request_id)),
            table_code(&short_id(&event.next_request_id)),
            event.lcp_units,
            event.lcp_bytes,
            event.retained_prefix_bytes,
            reuse,
            event.abandoned_prefix_bytes,
            event.recomputed_bytes,
            event.stable_after_switch_bytes,
            if event.domain_changed {
                locale.get("event_changed")?
            } else {
                locale.get("event_unchanged")?
            },
            table_code(&event.next_block_runs)
        )?;
    }
    output.push('\n');
    Ok(())
}

fn event_sequences(event: &SessionSwitchEvent, index: usize) -> (usize, usize) {
    (
        if event.previous_sequence == 0 {
            index + 1
        } else {
            event.previous_sequence
        },
        if event.next_sequence == 0 {
            index + 2
        } else {
            event.next_sequence
        },
    )
}

fn first_refresh_pair(report: &SessionReport) -> Option<(usize, usize)> {
    report
        .events
        .iter()
        .enumerate()
        .filter(|(_, event)| event_is_refresh(event))
        .map(|(index, event)| event_sequences(event, index))
        .min_by_key(|(previous, _)| *previous)
}

fn event_is_refresh(event: &SessionSwitchEvent) -> bool {
    event.prefix_reuse_outlier
        || (event.prefix_reuse_ratio.is_none()
            && !matches!(event.event_type, SessionEventType::Append))
}

fn effective_refresh_count(report: &SessionReport) -> usize {
    if report.context_refresh_count > 0 {
        report.context_refresh_count
    } else {
        report.fork_count + report.reorder_count + report.reset_count
    }
}

fn effective_abandoned_prefix_bytes(report: &SessionReport) -> usize {
    if report.context_refresh_count > 0 || report.total_abandoned_prefix_bytes > 0 {
        report.total_abandoned_prefix_bytes
    } else {
        report.total_prefix_cut_bytes
    }
}

fn event_tags(report: &SessionReport, locale: LocaleModule) -> Result<String> {
    let labels = [
        (report.append_count, "append"),
        (report.fork_count, "fork"),
        (report.reorder_count, "reorder"),
        (report.reset_count, "reset"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, key)| locale.get(key).map(|label| format!("{label}×{count}")))
    .collect::<Result<Vec<_>>>()?;
    Ok(if labels.is_empty() {
        "—".to_string()
    } else {
        labels.join(" / ")
    })
}

fn session_label(key: &str, common_prefix: &str) -> String {
    let stripped = key
        .strip_prefix(common_prefix)
        .unwrap_or(key)
        .trim_start_matches(['-', '_', '/']);
    let label = if stripped.chars().count() >= 8 {
        stripped.to_string()
    } else {
        let tail: Vec<char> = key.chars().collect();
        let start = tail.len().saturating_sub(20);
        format!("…{}", tail[start..].iter().collect::<String>())
    };
    let mut shown: String = label.chars().take(32).collect();
    if label.chars().count() > 32 {
        shown.push('…');
    }
    shown
}

fn common_key_prefix<'a>(keys: impl Iterator<Item = &'a str>) -> String {
    let mut common: Option<Vec<char>> = None;
    for key in keys {
        let chars: Vec<char> = key.chars().collect();
        common = Some(match common {
            None => chars,
            Some(current) => {
                let mut index = 0;
                while index < current.len() && index < chars.len() && current[index] == chars[index]
                {
                    index += 1;
                }
                current[..index].to_vec()
            }
        });
    }
    common.unwrap_or_default().into_iter().collect()
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn event_label(event: &SessionEventType, locale: LocaleModule) -> Result<&'static str> {
    locale.get(match event {
        SessionEventType::Append => "append",
        SessionEventType::Fork => "fork",
        SessionEventType::Reorder => "reorder",
        SessionEventType::Reset => "reset",
    })
}

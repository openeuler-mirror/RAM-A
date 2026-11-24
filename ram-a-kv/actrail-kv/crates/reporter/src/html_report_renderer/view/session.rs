//! 本文件把会话刷新证据、逐轮复用度和事件明细转换为展示模型。

use std::collections::BTreeSet;

use anyhow::Result;

use actrail_kv_artifacts::{SessionEventType, SessionReport, SessionSwitchEvent};

use super::super::format::{ratio, short_id};
use crate::locale::{namespace, LocaleModule, LocaleSnapshot};

pub(crate) struct SessionView {
    pub(crate) title: String,
    pub(crate) intro: String,
    pub(crate) metrics: Vec<MetricView>,
    pub(crate) note: String,
    pub(crate) endpoints_label: String,
    pub(crate) endpoints: String,
    pub(crate) models_label: String,
    pub(crate) models: String,
    pub(crate) common_prefix_note: Option<String>,
    pub(crate) no_refresh: Option<String>,
    pub(crate) attention: Option<SessionTable>,
    pub(crate) normal: Option<SessionTable>,
    pub(crate) labels: SessionLabels,
}

pub(crate) struct MetricView {
    pub(crate) value: String,
    pub(crate) label: String,
}

pub(crate) struct SessionTable {
    pub(crate) heading: String,
    pub(crate) collapsed: bool,
    pub(crate) include_detail: bool,
    pub(crate) rows: Vec<SessionRow>,
}

pub(crate) struct SessionLabels {
    pub(crate) table_session: String,
    pub(crate) table_requests: String,
    pub(crate) table_refreshes: String,
    pub(crate) table_first_refresh: String,
    pub(crate) table_events: String,
    pub(crate) table_min_reuse: String,
    pub(crate) table_abandoned: String,
    pub(crate) table_current_ratio: String,
    pub(crate) table_trend: String,
    pub(crate) detail: String,
    pub(crate) event_round: String,
    pub(crate) event_type: String,
    pub(crate) event_risk: String,
    pub(crate) event_previous: String,
    pub(crate) event_next: String,
    pub(crate) event_structural_prefix: String,
    pub(crate) event_retained_prefix: String,
    pub(crate) event_reuse_ratio: String,
    pub(crate) event_abandoned: String,
    pub(crate) event_recomputed: String,
    pub(crate) event_recoverable: String,
    pub(crate) event_domain: String,
    pub(crate) event_blocks: String,
}

pub(crate) struct SessionRow {
    pub(crate) class: &'static str,
    pub(crate) session: String,
    pub(crate) requests: usize,
    pub(crate) refreshes: usize,
    pub(crate) first_refresh: String,
    pub(crate) tags: Vec<TagView>,
    pub(crate) minimum_reuse: String,
    pub(crate) abandoned: usize,
    pub(crate) current_ratio: String,
    pub(crate) spark: Option<SparkView>,
    pub(crate) events: Vec<EventView>,
}

pub(crate) struct TagView {
    pub(crate) class: &'static str,
    pub(crate) label: String,
    pub(crate) count: usize,
}

pub(crate) struct SparkView {
    pub(crate) bars: Vec<SparkBarView>,
    pub(crate) range: String,
}

pub(crate) struct SparkBarView {
    pub(crate) class: &'static str,
    pub(crate) height: String,
}

pub(crate) struct EventView {
    pub(crate) round: String,
    pub(crate) event_type: String,
    pub(crate) risk: Option<String>,
    pub(crate) previous_request: String,
    pub(crate) next_request: String,
    pub(crate) structural_prefix: String,
    pub(crate) retained_prefix: String,
    pub(crate) reuse_ratio: String,
    pub(crate) abandoned: usize,
    pub(crate) recomputed: usize,
    pub(crate) recoverable: usize,
    pub(crate) domain: String,
    pub(crate) blocks: String,
}

pub(crate) fn build(reports: &[SessionReport], locale: LocaleSnapshot) -> Result<SessionView> {
    let l = locale.create(namespace::REPORT_SESSIONS)?;
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
    let common_prefix = common_key_prefix(reports.iter().map(|report| report.session_key.as_str()));
    let endpoints = reports
        .iter()
        .map(|report| report.endpoint_key.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(" / ");
    let models = reports
        .iter()
        .map(|report| report.model.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(" / ");
    let common_prefix_note = (!common_prefix.is_empty())
        .then(|| {
            l.format(
                "common_prefix_omitted",
                &[("prefix", common_prefix.clone())],
            )
        })
        .transpose()?;
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
    let attention = (!attention.is_empty())
        .then(|| {
            Ok::<SessionTable, anyhow::Error>(SessionTable {
                heading: l.format(
                    "warning_heading",
                    &[("sessions", attention.len().to_string())],
                )?,
                collapsed: false,
                include_detail: true,
                rows: build_rows(&attention, true, &common_prefix, locale)?,
            })
        })
        .transpose()?;
    let normal = if normal.is_empty() {
        None
    } else {
        let include_detail = normal.iter().any(|report| report.events.len() > 1);
        let collapsed = normal.len() > 8;
        let heading = if collapsed {
            l.format(
                "collapsed_detail_heading",
                &[("sessions", normal.len().to_string())],
            )?
        } else {
            l.format("detail_heading", &[("sessions", normal.len().to_string())])?
        };
        Some(SessionTable {
            heading,
            collapsed,
            include_detail,
            rows: build_rows(&normal, include_detail, &common_prefix, locale)?,
        })
    };
    Ok(SessionView {
        title: l.get("section_title")?.to_string(),
        intro: l.get("intro")?.to_string(),
        metrics: vec![
            MetricView {
                value: reports.len().to_string(),
                label: l.get("sessions")?.to_string(),
            },
            MetricView {
                value: requests.to_string(),
                label: l.get("requests")?.to_string(),
            },
            MetricView {
                value: refresh_count.to_string(),
                label: l.get("refresh_warnings")?.to_string(),
            },
            MetricView {
                value: refresh_sessions.to_string(),
                label: l.get("refresh_sessions")?.to_string(),
            },
            MetricView {
                value: format!("{:.1}%", ratio(lcp_bytes, next_bytes) * 100.0),
                label: l.get("current_ratio")?.to_string(),
            },
            MetricView {
                value: format!("{abandoned_bytes} B"),
                label: l.get("abandoned")?.to_string(),
            },
        ],
        note: l.get("note")?.to_string(),
        endpoints_label: l.get("endpoints")?.to_string(),
        endpoints,
        models_label: l.get("models")?.to_string(),
        models,
        common_prefix_note,
        no_refresh: attention
            .is_none()
            .then(|| l.get("no_refresh").map(str::to_string))
            .transpose()?,
        attention,
        normal,
        labels: session_labels(l)?,
    })
}

fn session_labels(l: LocaleModule) -> Result<SessionLabels> {
    Ok(SessionLabels {
        table_session: l.get("table_session")?.to_string(),
        table_requests: l.get("table_requests")?.to_string(),
        table_refreshes: l.get("table_refreshes")?.to_string(),
        table_first_refresh: l.get("table_first_refresh")?.to_string(),
        table_events: l.get("table_events")?.to_string(),
        table_min_reuse: l.get("table_min_reuse")?.to_string(),
        table_abandoned: l.get("table_abandoned")?.to_string(),
        table_current_ratio: l.get("table_current_ratio")?.to_string(),
        table_trend: l.get("table_trend")?.to_string(),
        detail: l.get("detail")?.to_string(),
        event_round: l.get("event_round")?.to_string(),
        event_type: l.get("event_type")?.to_string(),
        event_risk: l.get("event_risk")?.to_string(),
        event_previous: l.get("event_previous")?.to_string(),
        event_next: l.get("event_next")?.to_string(),
        event_structural_prefix: l.get("event_structural_prefix")?.to_string(),
        event_retained_prefix: l.get("event_retained_prefix")?.to_string(),
        event_reuse_ratio: l.get("event_reuse_ratio")?.to_string(),
        event_abandoned: l.get("event_abandoned")?.to_string(),
        event_recomputed: l.get("event_recomputed")?.to_string(),
        event_recoverable: l.get("event_recoverable")?.to_string(),
        event_domain: l.get("event_domain")?.to_string(),
        event_blocks: l.get("event_blocks")?.to_string(),
    })
}

fn build_rows(
    reports: &[&SessionReport],
    include_detail: bool,
    common_prefix: &str,
    locale: LocaleSnapshot,
) -> Result<Vec<SessionRow>> {
    let l = locale.create(namespace::REPORT_SESSIONS)?;
    reports
        .iter()
        .map(|report| {
            let lcp: usize = report.events.iter().map(|event| event.lcp_bytes).sum();
            let total: usize = report
                .events
                .iter()
                .map(|event| event.next_total_bytes)
                .sum();
            let refreshes = effective_refresh_count(report);
            let minimum_reuse = report
                .events
                .iter()
                .filter_map(|event| event.prefix_reuse_ratio)
                .reduce(f64::min)
                .map(|value| format!("{:.0}%", value * 100.0))
                .unwrap_or_else(|| "—".to_string());
            Ok(SessionRow {
                class: if refreshes > 0 { "sx-warn" } else { "" },
                session: session_label(&report.session_key, common_prefix),
                requests: report.request_count,
                refreshes,
                first_refresh: first_refresh_pair(report)
                    .map(|(previous, next)| format!("{previous} → {next}"))
                    .unwrap_or_else(|| "—".to_string()),
                tags: event_tags(report, l)?,
                minimum_reuse,
                abandoned: effective_abandoned_prefix_bytes(report),
                current_ratio: format!("{:.0}", ratio(lcp, total) * 100.0),
                spark: reuse_spark(report),
                events: if include_detail {
                    report
                        .events
                        .iter()
                        .enumerate()
                        .map(|(index, event)| build_event(event, index, l))
                        .collect::<Result<Vec<_>>>()?
                } else {
                    Vec::new()
                },
            })
        })
        .collect()
}

fn build_event(event: &SessionSwitchEvent, index: usize, l: LocaleModule) -> Result<EventView> {
    let (previous_sequence, next_sequence) = event_sequences(event, index);
    Ok(EventView {
        round: format!("{previous_sequence} → {next_sequence}"),
        event_type: event_label(&event.event_type, l)?.to_string(),
        risk: event_is_refresh(event)
            .then(|| l.get("event_refresh").map(str::to_string))
            .transpose()?,
        previous_request: short_id(&event.prev_request_id),
        next_request: short_id(&event.next_request_id),
        structural_prefix: format!("{} units / {} B", event.lcp_units, event.lcp_bytes),
        retained_prefix: format!("{} B", event.retained_prefix_bytes),
        reuse_ratio: event
            .prefix_reuse_ratio
            .map(|value| format!("{:.0}%", value * 100.0))
            .unwrap_or_else(|| "—".to_string()),
        abandoned: event.abandoned_prefix_bytes,
        recomputed: event.recomputed_bytes,
        recoverable: event.stable_after_switch_bytes,
        domain: if event.domain_changed {
            l.get("event_changed")?.to_string()
        } else {
            l.get("event_unchanged")?.to_string()
        },
        blocks: event.next_block_runs.clone(),
    })
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

pub(crate) fn first_refresh_pair(report: &SessionReport) -> Option<(usize, usize)> {
    report
        .events
        .iter()
        .enumerate()
        .filter(|(_, event)| event_is_refresh(event))
        .map(|(index, event)| event_sequences(event, index))
        .min_by_key(|(previous, _)| *previous)
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

fn reuse_spark(report: &SessionReport) -> Option<SparkView> {
    if report.events.is_empty() {
        return None;
    }
    let mut bars = Vec::new();
    let mut lowest = f64::MAX;
    let mut highest = f64::MIN;
    for event in &report.events {
        let value = event
            .prefix_reuse_ratio
            .unwrap_or_else(|| ratio(event.lcp_bytes, event.next_total_bytes));
        lowest = lowest.min(value);
        highest = highest.max(value);
        let class = if value < 0.5 {
            "sx-low"
        } else if value < 0.75 {
            "sx-mid"
        } else {
            "sx-high"
        };
        bars.push(SparkBarView {
            class,
            height: format!("{:.0}", (value * 100.0).clamp(8.0, 100.0)),
        });
    }
    Some(SparkView {
        bars,
        range: format!("{:.0}%~{:.0}%", lowest * 100.0, highest * 100.0),
    })
}

fn event_tags(report: &SessionReport, l: LocaleModule) -> Result<Vec<TagView>> {
    let tags = [
        (report.append_count, "sx-append", "append"),
        (report.fork_count, "sx-fork", "fork"),
        (report.reorder_count, "sx-reorder", "reorder"),
        (report.reset_count, "sx-reset", "reset"),
    ]
    .into_iter()
    .filter(|(count, _, _)| *count > 0)
    .map(|(count, class, key)| {
        Ok(TagView {
            class,
            label: l.get(key)?.to_string(),
            count,
        })
    })
    .collect::<Result<Vec<_>>>()?;
    Ok(tags)
}

fn event_is_refresh(event: &SessionSwitchEvent) -> bool {
    event.prefix_reuse_outlier
        || (event.prefix_reuse_ratio.is_none()
            && !matches!(event.event_type, SessionEventType::Append))
}

fn event_label(event: &SessionEventType, l: LocaleModule) -> Result<&'static str> {
    l.get(match event {
        SessionEventType::Append => "append",
        SessionEventType::Fork => "fork",
        SessionEventType::Reorder => "reorder",
        SessionEventType::Reset => "reset",
    })
}

//! 按 session 相邻对计算公共前缀、切换类型与可恢复稳定字节。

use std::collections::BTreeMap;

use actrail_kv_artifacts::{SessionEventType, SessionReport, SessionSwitchEvent};
use sha2::{Digest, Sha256};

use crate::model::projection::{
    CacheSequence, CacheUnit, CacheUnitKind, ComparisonDomain, ContextCollectionKind,
};

/// 会话内一行：session 标识与对应投影序列。
#[derive(Clone, Debug)]
pub(crate) struct SessionRow<'a> {
    pub session_key: &'a str,
    pub captured_at: Option<&'a str>,
    pub input_line: usize,
    pub sequence: &'a CacheSequence,
}

/// 按 session 分组，组内按采集时间稳定排序并输出刷新风险报告。
pub(crate) fn analyze_reports(rows: Vec<SessionRow<'_>>) -> Vec<SessionReport> {
    let mut by_session: BTreeMap<String, Vec<SessionRow<'_>>> = BTreeMap::new();
    for row in rows {
        by_session
            .entry(row.session_key.to_owned())
            .or_default()
            .push(row);
    }

    let mut reports: Vec<_> = by_session
        .into_iter()
        .filter(|(_, rows)| rows.len() >= 2)
        .map(|(session_key, mut rows)| {
            rows.sort_by(|left, right| {
                timestamp_key(left.captured_at)
                    .cmp(&timestamp_key(right.captured_at))
                    .then_with(|| left.input_line.cmp(&right.input_line))
            });
            build_report(session_key, rows)
        })
        .collect();
    reports.sort_by(|left, right| {
        right
            .context_refresh_count
            .cmp(&left.context_refresh_count)
            .then_with(|| {
                right
                    .total_abandoned_prefix_bytes
                    .cmp(&left.total_abandoned_prefix_bytes)
            })
            .then_with(|| left.session_key.cmp(&right.session_key))
    });
    reports
}

fn timestamp_key(value: Option<&str>) -> i128 {
    value
        .and_then(|value| {
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
        })
        .map(|value| value.unix_timestamp_nanos())
        .unwrap_or(i128::MIN)
}

fn build_report(session_key: String, rows: Vec<SessionRow<'_>>) -> SessionReport {
    let endpoint_key = rows[0].sequence.domain.endpoint_key.clone();
    let model = rows[0].sequence.domain.model.clone();
    let mut events = Vec::new();
    let mut append_count = 0usize;
    let mut fork_count = 0usize;
    let mut reorder_count = 0usize;
    let mut reset_count = 0usize;
    let mut context_refresh_count = 0usize;
    let mut domain_change_count = 0usize;
    let mut total_recomputed_bytes = 0usize;
    let mut total_stable_after_switch_bytes = 0usize;
    let mut total_prefix_cut_bytes = 0usize;
    let mut total_abandoned_prefix_bytes = 0usize;
    let mut previous_lcp_bytes: Option<usize> = None;
    for (index, pair) in rows.windows(2).enumerate() {
        let previous = pair[0].sequence;
        let next = pair[1].sequence;
        let (event_type, lcp_units, lcp_bytes) = classify(previous, next);
        let previous_pair_lcp_bytes = previous_lcp_bytes.unwrap_or(0);
        let prefix_cut_bytes = previous_pair_lcp_bytes.saturating_sub(lcp_bytes);
        previous_lcp_bytes = Some(lcp_bytes);
        let previous_total_bytes = observable_content_bytes(&previous.units);
        let next_total_bytes = observable_bytes(&next.units);
        let retained_prefix_bytes = content_prefix_bytes(&previous.units, &next.units);
        let abandoned_prefix_bytes = previous_total_bytes.saturating_sub(retained_prefix_bytes);
        let prefix_reuse_ratio = (previous_total_bytes > 0)
            .then(|| retained_prefix_bytes as f64 / previous_total_bytes as f64);
        let prefix_reuse_outlier =
            previous_total_bytes > 0 && retained_prefix_bytes < previous_total_bytes;
        let domain_changed = comparison_domain_changed(&previous.domain, &next.domain);
        let recomputed_bytes = next_total_bytes.saturating_sub(lcp_bytes);
        let stable_after_switch_bytes = recovered_contiguous_bytes(previous, next, lcp_units);
        match event_type {
            SessionEventType::Append => append_count += 1,
            SessionEventType::Fork => fork_count += 1,
            SessionEventType::Reorder => reorder_count += 1,
            SessionEventType::Reset => reset_count += 1,
        }
        context_refresh_count += usize::from(prefix_reuse_outlier);
        domain_change_count += usize::from(domain_changed);
        total_recomputed_bytes = total_recomputed_bytes.saturating_add(recomputed_bytes);
        total_stable_after_switch_bytes =
            total_stable_after_switch_bytes.saturating_add(stable_after_switch_bytes);
        total_prefix_cut_bytes = total_prefix_cut_bytes.saturating_add(prefix_cut_bytes);
        total_abandoned_prefix_bytes =
            total_abandoned_prefix_bytes.saturating_add(abandoned_prefix_bytes);
        events.push(SessionSwitchEvent {
            event_type,
            previous_sequence: index + 1,
            next_sequence: index + 2,
            prev_request_id: previous.request_id.clone(),
            next_request_id: next.request_id.clone(),
            lcp_units,
            lcp_bytes,
            previous_pair_lcp_bytes,
            prefix_cut_bytes,
            previous_total_bytes,
            retained_prefix_bytes,
            prefix_reuse_ratio,
            abandoned_prefix_bytes,
            prefix_reuse_outlier,
            domain_changed,
            next_total_bytes,
            recomputed_bytes,
            stable_after_switch_bytes,
            next_block_runs: run_length_summary(&next.units),
        });
    }
    let event_count = events.len();
    SessionReport {
        session_key,
        endpoint_key,
        model,
        request_count: rows.len(),
        append_count,
        fork_count,
        reorder_count,
        reset_count,
        context_refresh_count,
        total_recomputed_bytes,
        total_stable_after_switch_bytes,
        total_prefix_cut_bytes,
        avg_prefix_cut_bytes: total_prefix_cut_bytes.checked_div(event_count).unwrap_or(0),
        total_abandoned_prefix_bytes,
        domain_change_count,
        events,
    }
}

fn comparison_domain_changed(previous: &ComparisonDomain, next: &ComparisonDomain) -> bool {
    previous.endpoint_key != next.endpoint_key
        || previous.model != next.model
        || previous.model_deployment_key != next.model_deployment_key
        || previous.context_schema_key != next.context_schema_key
        || previous.agent_key != next.agent_key
        || previous.kv_namespace != next.kv_namespace
}

fn run_length_summary(units: &[CacheUnit]) -> String {
    let mut runs = Vec::new();
    let mut current: Option<(String, usize)> = None;
    for unit in units.iter().filter(|unit| is_content_bearing(unit)) {
        let digest = unit_digest(unit);
        match &mut current {
            Some((token, count)) if *token == digest => *count += 1,
            Some((token, count)) => {
                runs.push(format!("{token}x{count}"));
                current = Some((digest, 1));
            }
            None => current = Some((digest, 1)),
        }
    }
    if let Some((token, count)) = current {
        runs.push(format!("{token}x{count}"));
    }
    runs.join("/")
}

fn unit_digest(unit: &CacheUnit) -> String {
    let digest = Sha256::digest(unit.content.as_bytes());
    digest
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn classify(prev: &CacheSequence, next: &CacheSequence) -> (SessionEventType, usize, usize) {
    let mut lcp = 0usize;
    while lcp < prev.units.len()
        && lcp < next.units.len()
        && same_observable(&prev.units[lcp], &next.units[lcp])
    {
        lcp += 1;
    }
    let lcp_bytes = prev.units[..lcp]
        .iter()
        .map(|unit| unit.content.len())
        .sum();
    let prev_content_len = prev
        .units
        .iter()
        .filter(|unit| is_content_bearing(unit))
        .count();
    let next_content_len = next
        .units
        .iter()
        .filter(|unit| is_content_bearing(unit))
        .count();
    if lcp == prev.units.len()
        || (content_prefix_units(&prev.units, &next.units) == prev_content_len
            && next_content_len > prev_content_len)
    {
        return (SessionEventType::Append, lcp, lcp_bytes);
    }
    if unique_permutation(&prev.units, &next.units) {
        return (SessionEventType::Reorder, lcp, lcp_bytes);
    }
    if content_prefix_units(&prev.units, &next.units) == 0 {
        (SessionEventType::Reset, lcp, lcp_bytes)
    } else {
        (SessionEventType::Fork, lcp, lcp_bytes)
    }
}

/// 内容性单位从请求起始连续相同的个数（结构/role 标记不参与 reset 判定）。
fn content_prefix_units(prev: &[CacheUnit], next: &[CacheUnit]) -> usize {
    let prev_content: Vec<_> = prev
        .iter()
        .filter(|unit| is_content_bearing(unit))
        .collect();
    let next_content: Vec<_> = next
        .iter()
        .filter(|unit| is_content_bearing(unit))
        .collect();
    let mut count = 0usize;
    while count < prev_content.len()
        && count < next_content.len()
        && same_observable(prev_content[count], next_content[count])
    {
        count += 1;
    }
    count
}

fn same_observable(left: &CacheUnit, right: &CacheUnit) -> bool {
    left.kind == right.kind
        && left.alignment_key == right.alignment_key
        && left.content == right.content
        && left.hierarchy.collection == right.hierarchy.collection
}

/// 仅在内容性单位多重集相同且各自单位唯一时声称 reorder（role/结构噪声不参与）。
fn unique_permutation(prev: &[CacheUnit], next: &[CacheUnit]) -> bool {
    let prev_content: Vec<_> = prev
        .iter()
        .filter(|unit| is_content_bearing(unit))
        .collect();
    let next_content: Vec<_> = next
        .iter()
        .filter(|unit| is_content_bearing(unit))
        .collect();
    if prev_content.len() != next_content.len() || prev_content.is_empty() {
        return false;
    }
    let mut prev_counts: BTreeMap<UnitFingerprint, usize> = BTreeMap::new();
    let mut next_counts: BTreeMap<UnitFingerprint, usize> = BTreeMap::new();
    for unit in &prev_content {
        prev_counts
            .entry(fingerprint(unit))
            .and_modify(|count| *count += 1)
            .or_insert(1);
    }
    for unit in &next_content {
        next_counts
            .entry(fingerprint(unit))
            .and_modify(|count| *count += 1)
            .or_insert(1);
    }
    prev_counts.len() == prev_content.len() && prev_counts == next_counts
}

fn is_content_bearing(unit: &CacheUnit) -> bool {
    matches!(
        unit.kind,
        CacheUnitKind::VisibleText
            | CacheUnitKind::ToolDefinition
            | CacheUnitKind::ToolCall
            | CacheUnitKind::ToolResult
    )
}

type UnitFingerprint = (CacheUnitKind, String, String, ContextCollectionKind);

fn fingerprint(unit: &CacheUnit) -> UnitFingerprint {
    (
        unit.kind.clone(),
        unit.alignment_key.clone(),
        unit.content.clone(),
        unit.hierarchy.collection.clone(),
    )
}

/// 第一个分歧点之后，两条请求按原顺序连续对齐的最长相同单位段字节数（CC 证据）。
fn recovered_contiguous_bytes(prev: &CacheSequence, next: &CacheSequence, lcp: usize) -> usize {
    let mut best_bytes = 0usize;
    let mut best_units = 0usize;
    for next_start in lcp..next.units.len() {
        let mut run_bytes = 0usize;
        let mut run_units = 0usize;
        let mut prev_cursor = lcp;
        for unit in &next.units[next_start..] {
            let matched =
                prev_cursor < prev.units.len() && same_observable(unit, &prev.units[prev_cursor]);
            if matched {
                run_bytes += unit.content.len();
                run_units += 1;
                prev_cursor += 1;
                if run_bytes > best_bytes {
                    best_bytes = run_bytes;
                    best_units = run_units;
                }
            } else {
                run_bytes = 0;
                run_units = 0;
                prev_cursor = lcp;
            }
        }
    }
    let _ = best_units;
    best_bytes
}

fn observable_bytes(units: &[CacheUnit]) -> usize {
    units.iter().map(|unit| unit.content.len()).sum()
}

fn observable_content_bytes(units: &[CacheUnit]) -> usize {
    units
        .iter()
        .filter(|unit| is_content_bearing(unit))
        .map(|unit| unit.content.len())
        .sum()
}

/// 内容性单元的连续公共前缀字节；结构起止标记的移动不算上下文刷新。
fn content_prefix_bytes(previous: &[CacheUnit], next: &[CacheUnit]) -> usize {
    let common = content_prefix_units(previous, next);
    previous
        .iter()
        .filter(|unit| is_content_bearing(unit))
        .take(common)
        .map(|unit| unit.content.len())
        .sum()
}

//! 本文件提供 HTML 报告展示模型使用的通用格式化和安全截断函数。

use actrail_kv_artifacts::ContextDefect;

/// 报告里的字节量按 KB / MB 展示，避免长数字串。
pub(super) fn human_bytes(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let value = bytes as f64;
    if value >= MB {
        format!("{:.1} MB", value / MB)
    } else if value >= KB {
        format!("{:.0} KB", value / KB)
    } else {
        format!("{bytes} B")
    }
}

/// 请求和会话 ID 只需可区分，截断展示避免长串淹没其它字段。
pub(super) fn short_id(id: &str) -> String {
    let mut short: String = id.chars().take(12).collect();
    if id.chars().count() > 12 {
        short.push('…');
    }
    short
}

/// 行内摘要压缩空白并截断，完整文本仍保留在展开体中。
pub(super) fn excerpt_label(text: &str, limit: usize) -> String {
    let unescaped = text
        .replace("\\r\\n", " ")
        .replace("\\n", " ")
        .replace("\\r", " ")
        .replace("\\t", " ");
    let flat = unescaped.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut label: String = flat.chars().take(limit).collect();
    if flat.chars().count() > limit {
        label.push('…');
    }
    label
}

pub(super) fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

pub(super) fn defect_location(
    defect: &ContextDefect,
    role_label: &str,
    unit_label: &str,
) -> String {
    let source = defect
        .mismatch
        .variants
        .iter()
        .find_map(|variant| variant.representative.sources.first());
    let Some(source) = source else {
        return "—".to_string();
    };
    let mut location = source.json_path.clone();
    if let Some(role) = source.role.as_deref() {
        location.push_str(&format!(" ({role_label}={role})"));
    }
    if let Some(unit) = source.unit_index {
        location.push_str(&format!(" · {unit_label} #{unit}"));
    }
    location
}

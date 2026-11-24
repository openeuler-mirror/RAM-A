//! 本文件提供 GFM 文本、表格、代码片段、锚点和数值格式辅助。

pub(super) fn inline(value: &str) -> String {
    let flattened = flatten(value);
    let mut escaped = String::with_capacity(flattened.len());
    for character in flattened.chars() {
        match character {
            '\\' | '*' | '_' | '[' | ']' | '`' | '#' | '|' | '~' => {
                escaped.push('\\');
                escaped.push(character);
            }
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

pub(super) fn code(value: &str) -> String {
    let flattened = flatten(value);
    let max_run = longest_backtick_run(&flattened);
    let fence = "`".repeat(max_run + 1);
    let padding = if flattened.starts_with(['`', ' ']) || flattened.ends_with(['`', ' ']) {
        " "
    } else {
        ""
    };
    format!("{fence}{padding}{flattened}{padding}{fence}")
}

pub(super) fn table_code(value: &str) -> String {
    code(&value.replace('|', "\\|"))
}

pub(super) fn fenced(value: &str) -> String {
    let max_run = longest_backtick_run(value);
    let fence = "`".repeat((max_run + 1).max(3));
    format!("{fence}text\n{value}\n{fence}")
}

pub(super) fn flatten(value: &str) -> String {
    value
        .replace("\\r\\n", " ")
        .replace("\\n", " ")
        .replace("\\r", " ")
        .replace("\\t", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn excerpt(value: &str, limit: usize) -> String {
    let flattened = flatten(value);
    let mut excerpt: String = flattened.chars().take(limit).collect();
    if flattened.chars().count() > limit {
        excerpt.push('…');
    }
    excerpt
}

pub(super) fn short_id(id: &str) -> String {
    let mut short: String = id.chars().take(12).collect();
    if id.chars().count() > 12 {
        short.push('…');
    }
    short
}

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

pub(super) fn heading_anchor(value: &str) -> String {
    let mut anchor = String::new();
    let mut separator = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() || character == '-' || character == '_' {
            if separator && !anchor.is_empty() && !anchor.ends_with('-') {
                anchor.push('-');
            }
            separator = false;
            anchor.push(character);
        } else {
            separator = true;
        }
    }
    anchor.trim_matches('-').to_string()
}

fn longest_backtick_run(value: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for byte in value.bytes() {
        if byte == b'`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

#[cfg(test)]
#[path = "../../../../tests/unit/reporter/markdown_report_renderer/format.rs"]
mod tests;

//! 本文件验证 markdown_report_renderer/format 模块的行为。

use super::*;

#[test]
fn escapes_markdown_and_raw_html() {
    let escaped = inline("<b>| `x` *y* [z]</b>");
    assert_eq!(escaped, "&lt;b&gt;\\| \\`x\\` \\*y\\* \\[z\\]&lt;/b&gt;");
}

#[test]
fn code_fence_grows_past_existing_backticks() {
    assert_eq!(code("a `b` c"), "``a `b` c``");
    assert_eq!(table_code("a|b"), "`a\\|b`");
}

#[test]
fn heading_anchor_is_stable_for_ids_and_chinese() {
    assert_eq!(heading_anchor("缺陷 defect-a"), "缺陷-defect-a");
    assert_eq!(heading_anchor("Template  abc_1"), "template-abc_1");
}

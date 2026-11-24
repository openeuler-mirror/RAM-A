//! 本文件验证 locale/snapshot 模块的行为。

use super::LocaleSnapshot;
use crate::locale::Locale;

#[test]
fn namespace_keys_are_scoped_and_missing_keys_fail() {
    let locale = LocaleSnapshot::new(Locale::ZhCn);
    let defects = locale.create("report.defects").expect("defects");
    assert!(defects.get("section_title").is_ok());
    assert!(defects.get("not_registered").is_err());
    assert!(locale.create("report.unknown").is_err());
}

#[test]
fn formatting_treats_parameter_values_as_literal_text() {
    let locale = LocaleSnapshot::new(Locale::ZhCn);
    let common = locale.create("report.common").expect("common locale");

    let rendered = common
        .format(
            "summary",
            &[
                ("input", r#"{"name":"x"}"#.to_string()),
                ("analyzed", "1".to_string()),
                ("skipped", "{skipped}".to_string()),
                ("defects", "0".to_string()),
            ],
        )
        .expect("format literal braces");

    assert!(rendered.contains(r#"{"name":"x"}"#));
    assert!(rendered.contains("{skipped}"));
}

#[test]
fn formatting_rejects_missing_parameters() {
    let locale = LocaleSnapshot::new(Locale::ZhCn);
    let common = locale.create("report.common").expect("common locale");
    let error = common
        .format("summary", &[("input", "1".to_string())])
        .expect_err("missing parameters must fail");

    assert!(error.to_string().contains("missing parameter analyzed"));
}

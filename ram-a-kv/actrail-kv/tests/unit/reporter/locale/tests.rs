//! 本文件验证 locale 模块的行为。

use super::Locale;

pub(crate) static LOCALE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn parses_canonical_and_alias_locale_names() {
    for value in ["zh-CN", "zh_cn", "ZH-cn", "zh"] {
        assert_eq!(Locale::parse(value), Some(Locale::ZhCn));
    }
    for value in ["en-US", "en_us", "EN-us", "en"] {
        assert_eq!(Locale::parse(value), Some(Locale::EnUs));
    }
    assert_eq!(Locale::parse("fr-FR"), None);
}

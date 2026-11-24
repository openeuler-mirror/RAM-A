//! 本文件验证 locale/factory 模块的行为。

use super::LocaleFactory;
use crate::locale::{Locale, LOCALE_TEST_LOCK};

#[test]
fn locale_change_only_affects_new_snapshots() {
    let _guard = LOCALE_TEST_LOCK.lock().expect("locale test lock");
    LocaleFactory::set_locale(Locale::ZhCn).expect("zh");
    let zh = LocaleFactory::snapshot().expect("zh snapshot");
    let zh_common = LocaleFactory::create("report.common").expect("zh module");
    LocaleFactory::set_locale(Locale::EnUs).expect("en");
    let en = LocaleFactory::snapshot().expect("en snapshot");

    assert_eq!(zh.locale(), Locale::ZhCn);
    assert_eq!(en.locale(), Locale::EnUs);
    assert_eq!(zh.locale(), Locale::ZhCn);
    assert_eq!(
        zh_common.get("document_title").expect("zh title"),
        "LLM KV 缓存结构诊断"
    );
    assert_ne!(
        zh_common.get("document_title").expect("zh title"),
        LocaleFactory::create("report.common")
            .expect("en module")
            .get("document_title")
            .expect("en title")
    );
    LocaleFactory::set_locale(Locale::ZhCn).expect("restore");
}

#[test]
fn unsupported_locale_uses_registered_cli_message() {
    let _guard = LOCALE_TEST_LOCK.lock().expect("locale test lock");
    LocaleFactory::set_locale(Locale::ZhCn).expect("zh");

    let error = LocaleFactory::init_from_code("fr-FR").expect_err("unsupported locale");
    let message = error.to_string();

    assert!(message.contains("不支持 locale fr-FR"));
    assert!(message.contains("zh-CN, en-US"));
}

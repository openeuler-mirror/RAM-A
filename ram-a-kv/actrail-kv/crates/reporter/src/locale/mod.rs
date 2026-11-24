//! 本文件定义报告 locale、namespace 公共常量和工厂入口。

mod catalogs;
mod factory;
mod registry;
mod snapshot;

pub use factory::LocaleFactory;
pub use snapshot::{LocaleModule, LocaleSnapshot};

/// 报告支持的首期 locale。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Locale {
    ZhCn,
    EnUs,
}

impl Locale {
    pub const fn code(self) -> &'static str {
        match self {
            Self::ZhCn => "zh-CN",
            Self::EnUs => "en-US",
        }
    }

    /// 解析 locale，允许大小写差异和下划线/连字符等价。
    pub fn parse(value: &str) -> Option<Self> {
        let normalized = value.trim().replace('_', "-").to_ascii_lowercase();
        match normalized.as_str() {
            "zh" | "zh-cn" => Some(Self::ZhCn),
            "en" | "en-us" => Some(Self::EnUs),
            _ => None,
        }
    }

    pub const fn supported_codes() -> &'static [&'static str] {
        &["zh-CN", "en-US"]
    }
}

pub mod namespace {
    pub const REPORT_COMMON: &str = "report.common";
    pub const REPORT_PREFIX_REUSE: &str = "report.prefix_reuse";
    pub const REPORT_TOP_K: &str = "report.top_k";
    pub const REPORT_DEFECTS: &str = "report.defects";
    pub const REPORT_SESSIONS: &str = "report.sessions";
    pub const REPORT_CACHE: &str = "report.cache";
    pub const REPORT_PREFIX_TREE: &str = "report.prefix_tree";
    pub const CLI_REPORT: &str = "cli.report";

    pub const ALL: &[&str] = &[
        REPORT_COMMON,
        REPORT_PREFIX_REUSE,
        REPORT_TOP_K,
        REPORT_DEFECTS,
        REPORT_SESSIONS,
        REPORT_CACHE,
        REPORT_PREFIX_TREE,
        CLI_REPORT,
    ];
}

#[cfg(test)]
pub(crate) use self::tests::LOCALE_TEST_LOCK;

#[cfg(test)]
#[path = "../../../../tests/unit/reporter/locale/tests.rs"]
mod tests;

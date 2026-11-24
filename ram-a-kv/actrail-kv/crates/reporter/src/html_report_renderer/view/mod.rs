//! 本文件定义 HTML 模板消费的完整展示模型，并完成根页面数据装配。

use anyhow::Result;

use actrail_kv_artifacts::AnalysisResult;

use crate::locale::{namespace, LocaleSnapshot};

pub(crate) mod cache;
pub(crate) mod defects;
pub(crate) mod prefix_reuse;
pub(crate) mod prefix_tree;
pub(crate) mod session;

pub(crate) use cache::CacheView;
pub(crate) use defects::{DefectsView, TopKView};
pub(crate) use prefix_reuse::PrefixReuseView;
pub(crate) use prefix_tree::PrefixTreeView;
pub(crate) use session::SessionView;

pub(crate) struct ReportView {
    pub(crate) lang: &'static str,
    pub(crate) title: String,
    pub(crate) document_title: String,
    pub(crate) summary: String,
    pub(crate) limitations: String,
    pub(crate) prefix_reuse: Option<PrefixReuseView>,
    pub(crate) top_k: TopKView,
    pub(crate) defects: DefectsView,
    pub(crate) sessions: Option<SessionView>,
    pub(crate) cache: Option<CacheView>,
    pub(crate) prefix_tree: Option<PrefixTreeView>,
}

impl ReportView {
    pub(crate) fn build(result: &AnalysisResult, locale: LocaleSnapshot) -> Result<Self> {
        let common = locale.create(namespace::REPORT_COMMON)?;
        Ok(Self {
            lang: locale.locale().code(),
            title: common.get("html_title")?.to_string(),
            document_title: common.get("document_title")?.to_string(),
            summary: common.format(
                "summary",
                &[
                    ("input", result.run.input_records.to_string()),
                    ("analyzed", result.run.analyzed_records.to_string()),
                    ("skipped", result.run.skipped_records.len().to_string()),
                    ("defects", result.defects.len().to_string()),
                ],
            )?,
            limitations: common.get("limitations")?.to_string(),
            prefix_reuse: result
                .prefix_reuse
                .as_ref()
                .map(|summary| prefix_reuse::build(summary, &result.cache_metrics, locale))
                .transpose()?,
            top_k: defects::build_top_k(result, locale)?,
            defects: defects::build_defects(result, locale)?,
            sessions: (!result.session_reports.is_empty())
                .then(|| session::build(&result.session_reports, locale))
                .transpose()?,
            cache: (!result.cache_metrics.is_empty())
                .then(|| cache::build(result, locale))
                .transpose()?,
            prefix_tree: prefix_tree::build(result, locale)?,
        })
    }
}

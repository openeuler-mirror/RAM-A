//! 本 crate 定义三个二进制之间稳定、可序列化的磁盘数据契约。

mod analysis_result;
mod captured_request;

pub use analysis_result::{
    AnalysisOptionsSnapshot, AnalysisResult, AnalysisRunSummary, CacheMetricBasis, ComparisonGroup,
    ContextDefect, DefectFact, DefectFactKind, KvCacheMetric, MismatchPattern, MismatchRegion,
    MismatchVariant, OptimizationInsight, PrefixNode, PrefixNodeKind, PrefixReuseComposition,
    PrefixReuseRequest, PrefixReuseSession, PrefixReuseSummary, PrefixVariant, RecoveredStable,
    RequestTemplate, ScoreBreakdown, SessionEventType, SessionReport, SessionSwitchEvent,
    SkipRecord, SourceLocation, StableSpan, TemplatePrefixView, TemplateSlot, VariantEvidence,
    ANALYSIS_SCHEMA_VERSION,
};
pub use captured_request::{CapturedRequest, ComparisonMetadata};

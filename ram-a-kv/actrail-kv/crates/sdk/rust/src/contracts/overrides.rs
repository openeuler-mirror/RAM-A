//! Re-exports generated configuration overrides without linking business implementations.
pub use analysis_contract::config::{
    candidate::CandidateConfigOverrides, corpus::CorpusConfigOverrides,
    diagnosis::DiagnosisConfigOverrides, prefix_view::PrefixViewConfigOverrides,
    projection::ProjectionConfigOverrides, stability::StabilityConfigOverrides,
    template::TemplateConfigOverrides, AnalyzeConfigOverrides,
};
pub use analysis_contract::settings::{
    AnalyzerSettingsOverrides, SelectionSettingsOverrides, TimeRangeSettings,
};
pub use report_contract::settings::ReportSettingsOverrides;
pub use storage_contract::{
    NdjsonConfigOverrides, NdjsonRetentionConfigOverrides, SqliteConfigOverrides,
    SqliteRetentionConfigOverrides, StorageBackendConfigsOverrides, StorageBackendKind,
    StorageConfigOverrides, StorageRetentionConfigOverrides,
};

#[derive(Default, serde::Serialize)]
pub struct SdkOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<StorageConfigOverrides>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analyzer: Option<AnalyzerSettingsOverrides>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<ReportSettingsOverrides>,
}

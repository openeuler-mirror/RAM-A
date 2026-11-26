//! 递归组合各分析子模块配置，作为执行与结果快照的共同类型。
use anyhow::{ensure, Result};
use config_contract::{Config, RootConfig};
use serde::Serialize;
pub mod corpus;
pub use corpus::CorpusConfig;
pub mod projection;
pub use projection::ProjectionConfig;
pub mod candidate;
pub use candidate::CandidateConfig;
pub mod stability;
pub use stability::StabilityConfig;
pub mod template;
pub use template::TemplateConfig;
pub mod diagnosis;
pub use diagnosis::DiagnosisConfig;
pub mod prefix_view;
pub use prefix_view::PrefixViewConfig;

#[derive(Clone, Debug, PartialEq, Serialize, Config)]
#[serde(default)]
pub struct AnalyzeConfig {
    /// 排名输出条数
    pub top_k: usize,
    /// 语料加载配置
    pub corpus: CorpusConfig,
    /// 请求投影配置
    pub projection: ProjectionConfig,
    /// 候选构造配置
    pub candidate: CandidateConfig,
    /// 模板与诊断共享的稳定性条件配置
    pub stability: StabilityConfig,
    /// 模板提取配置
    pub template: TemplateConfig,
    /// 缺陷诊断配置
    pub diagnosis: DiagnosisConfig,
    /// 前缀树视图配置
    pub prefix_view: PrefixViewConfig,
}
impl RootConfig for AnalyzeConfig {}
impl Default for AnalyzeConfig {
    fn default() -> Self {
        Self {
            top_k: 20,
            corpus: Default::default(),
            projection: Default::default(),
            candidate: Default::default(),
            stability: Default::default(),
            template: Default::default(),
            diagnosis: Default::default(),
            prefix_view: Default::default(),
        }
    }
}
impl AnalyzeConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.top_k > 0, "top_k must be greater than zero");
        self.corpus.validate()?;
        self.projection.validate()?;
        self.candidate.validate()?;
        self.stability.validate()?;
        self.template.validate()?;
        self.diagnosis.validate()?;
        self.prefix_view.validate()?;
        Ok(())
    }
}

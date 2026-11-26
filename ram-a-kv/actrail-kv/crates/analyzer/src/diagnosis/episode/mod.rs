//! 本模块公开 region-first P1/X/P2 定位、成员变体直方图与缺陷候选类型。

pub(in crate::diagnosis) mod locator;
mod model;
pub(in crate::diagnosis) mod variant;

pub use locator::diagnose_template;
pub use model::{DefectCandidate, DiagnosisConfig, EpisodeVariant};

#[cfg(test)]
#[path = "../../../../../tests/unit/analyzer/diagnosis/episode/tests.rs"]
mod tests;

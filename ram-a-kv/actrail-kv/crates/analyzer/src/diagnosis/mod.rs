//! 本模块从模板符号坐标统一定位 P1/X/P2 episode，并为每个 region 生成一次聚合候选。

pub mod episode;
mod facts;
mod identity;

pub use episode::{diagnose_template, DefectCandidate, DiagnosisOptions, EpisodeVariant};

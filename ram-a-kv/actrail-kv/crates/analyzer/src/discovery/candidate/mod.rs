//! Candidate cohorts prevent cross-domain comparison and transitive template over-merging.

mod cohort;
mod signature;

pub use cohort::{
    CandidateBuildResult, CandidateBuilder, CandidateCohort, CandidateOptions, CandidateSkip,
};
pub use signature::{
    bounded_text_similarity, compatibility, dynamic_coverage_ratio, pair_metrics, PairMetrics,
    StructureSignature,
};

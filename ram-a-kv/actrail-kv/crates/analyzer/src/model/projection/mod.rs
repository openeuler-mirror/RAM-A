//! OpenAI-compatible projection into observable ordered cache units with source provenance.

mod projector;
mod types;

pub use projector::{ProjectionLimits, RequestProjector};
pub use types::{
    CacheSequence, CacheUnit, CacheUnitKind, ComparisonDomain, ContextCollectionKind,
    HierarchyLocation, ProjectionSkip, ProjectionSkipReason, SourceLocation,
};

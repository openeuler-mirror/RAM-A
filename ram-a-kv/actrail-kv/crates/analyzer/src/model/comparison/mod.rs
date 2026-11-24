//! Comparison groups isolate requests that cannot share the same model-visible prefix cache.

mod group_key;

pub use group_key::{ComparisonGroupError, ComparisonGroupKey};

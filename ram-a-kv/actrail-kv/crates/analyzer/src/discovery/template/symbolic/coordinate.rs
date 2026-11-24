//! Template coordinates identify stable and variable fragments independently of absolute unit indexes.

use std::ops::Range;

use crate::model::projection::{CacheUnitKind, ContextCollectionKind, SourceLocation};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TemplateCoordinateId(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoordinateKind {
    Stable,
    Slot,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogicalUnitKey {
    pub collection: ContextCollectionKind,
    pub unit_kind: CacheUnitKind,
    pub alignment_key: String,
    pub role: Option<String>,
    pub stable_identity: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TemplateCoordinate {
    pub id: TemplateCoordinateId,
    pub ordinal: usize,
    pub kind: CoordinateKind,
    pub logical_unit: LogicalUnitKey,
    pub medoid_unit_index: usize,
    pub medoid_utf8_bytes: Range<usize>,
    pub source: SourceLocation,
    pub content_digest: String,
    pub observable_bytes: usize,
    pub support_count: usize,
    pub support_ratio: f64,
}

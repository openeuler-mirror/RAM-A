//! Extracted templates expose stable evidence, dynamic slots, members, and quality measures.

use std::ops::Range;

use crate::model::projection::{CacheSequence, ComparisonDomain, SourceLocation};

use super::symbolic::{MemberCoordinateMap, SymbolicMemberSequence, TemplateCoordinate};

#[derive(Clone, Debug)]
pub struct StableSpan {
    pub unit_index: usize,
    pub content: String,
    pub medoid_utf8_bytes: Range<usize>,
    pub source: SourceLocation,
    pub support_count: usize,
    pub support_ratio: f64,
}

#[derive(Clone, Debug)]
pub struct TemplateSlot {
    pub unit_index: usize,
    pub medoid_utf8_bytes: Range<usize>,
    pub source: SourceLocation,
    pub observed_values: Vec<String>,
    pub distinct_variant_count: usize,
    pub support_count: usize,
}

#[derive(Clone, Debug)]
pub struct RequestTemplate {
    pub id: String,
    pub domain: ComparisonDomain,
    pub medoid_request_id: String,
    pub members: Vec<CacheSequence>,
    pub stable_spans: Vec<StableSpan>,
    pub slots: Vec<TemplateSlot>,
    pub coordinates: Vec<TemplateCoordinate>,
    pub member_maps: Vec<MemberCoordinateMap>,
    pub symbolic_members: Vec<SymbolicMemberSequence>,
    pub cohesion: f64,
    pub projection_reliability: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateSkip {
    pub cohort_id: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default)]
pub struct TemplateExtractionResult {
    pub templates: Vec<RequestTemplate>,
    pub skipped: Vec<TemplateSkip>,
}

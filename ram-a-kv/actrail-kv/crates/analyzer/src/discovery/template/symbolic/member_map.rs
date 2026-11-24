//! Member coordinate maps retain present fragments, explicit gaps, and inserted unmatched regions.

use std::ops::Range;

use crate::model::projection::SourceLocation;

use super::TemplateCoordinateId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberFragmentRef {
    pub unit_index: usize,
    pub utf8_bytes: Range<usize>,
    pub source: SourceLocation,
    pub observable_bytes: usize,
    pub content_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoordinateBindingState {
    Present(MemberFragmentRef),
    Gap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoordinateBinding {
    pub coordinate_id: TemplateCoordinateId,
    pub state: CoordinateBindingState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnmatchedRun {
    pub run_id: String,
    pub left_coordinate_id: Option<TemplateCoordinateId>,
    pub right_coordinate_id: Option<TemplateCoordinateId>,
    pub fragments: Vec<MemberFragmentRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberCoordinateMap {
    pub member_request_id: String,
    pub bindings: Vec<CoordinateBinding>,
    pub unmatched_runs: Vec<UnmatchedRun>,
}

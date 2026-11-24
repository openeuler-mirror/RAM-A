//! Symbolic member sequences are the ordered diagnosis input in shared template coordinates.

use super::{MemberFragmentRef, TemplateCoordinateId, UnmatchedRun};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SymbolicAtom {
    StableRef {
        coordinate_id: TemplateCoordinateId,
        fragment: MemberFragmentRef,
    },
    SlotBinding {
        coordinate_id: TemplateCoordinateId,
        variant_id: String,
        fragment: MemberFragmentRef,
    },
    UnmatchedRun(UnmatchedRun),
    Gap {
        coordinate_id: TemplateCoordinateId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolicMemberSequence {
    pub member_request_id: String,
    pub atoms: Vec<SymbolicAtom>,
    pub observable_bytes: usize,
}

//! Symbolic template coordinates preserve every member's binding, gaps, and unmatched runs.

mod build;
mod coordinate;
mod identity;
mod member_map;
mod sequence;

pub use coordinate::{CoordinateKind, LogicalUnitKey, TemplateCoordinate, TemplateCoordinateId};
pub use member_map::{
    CoordinateBinding, CoordinateBindingState, MemberCoordinateMap, MemberFragmentRef, UnmatchedRun,
};
pub use sequence::{SymbolicAtom, SymbolicMemberSequence};

pub(crate) use build::build_symbolic_template;

#[cfg(test)]
#[path = "../../../../../../tests/unit/analyzer/discovery/template/symbolic/tests.rs"]
mod tests;

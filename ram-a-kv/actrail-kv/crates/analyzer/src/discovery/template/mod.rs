//! Unit alignment and byte-safe stable-span/slot template extraction.

mod align;
mod extractor;
pub mod symbolic;
mod types;

pub use align::{AlignmentError, SequenceAligner, UnitAlignment};
pub use extractor::{TemplateExtractor, TemplateOptions};
pub use symbolic::{
    CoordinateBinding, CoordinateBindingState, CoordinateKind, LogicalUnitKey, MemberCoordinateMap,
    MemberFragmentRef, SymbolicAtom, SymbolicMemberSequence, TemplateCoordinate,
    TemplateCoordinateId, UnmatchedRun,
};
pub use types::{
    RequestTemplate, StableSpan, TemplateExtractionResult, TemplateSkip, TemplateSlot,
};

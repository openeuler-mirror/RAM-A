//! 本文件验证 diagnosis/facts/sequence 模块的行为。

use actrail_kv_artifacts::SourceLocation;

use super::*;
use crate::diagnosis::episode::variant::LocatedPart;

#[test]
fn recognizes_only_unique_identity_permutations_as_reorder() {
    let forward = variant(&["lookup", "calculate"]);
    let reverse = variant(&["calculate", "lookup"]);
    assert!(analyze(&[forward, reverse]).reorder);

    let duplicate = variant(&["lookup", "lookup"]);
    assert!(!analyze(&[variant(&["lookup", "calculate"]), duplicate]).reorder);
}

fn variant(identities: &[&str]) -> LocatedVariant {
    LocatedVariant {
        member_id: identities.join("-"),
        fingerprint: identities.join("/"),
        shape: "binding/binding".into(),
        parts: identities
            .iter()
            .map(|identity| LocatedPart {
                coordinate_id: None,
                content_digest: (*identity).into(),
                content: (*identity).into(),
                source: SourceLocation {
                    json_path: "$.tools".into(),
                    logical_scope: vec![],
                    unit_index: None,
                    byte_start: None,
                    byte_end: None,
                    role: None,
                },
                observable_bytes: identity.len(),
                stable_identity: Some((*identity).into()),
                kind: "binding",
            })
            .collect(),
        text: identities.concat(),
        has_gap_or_unmatched: false,
        actual_prefix_bytes: 0,
    }
}

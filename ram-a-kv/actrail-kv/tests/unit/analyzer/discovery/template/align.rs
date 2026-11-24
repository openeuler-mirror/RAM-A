//! 本文件验证 discovery/template/align 模块的行为。

use super::*;
use crate::model::projection::{
    CacheUnitKind, ContextCollectionKind, HierarchyLocation, SourceLocation,
};

fn unit(key: &str, value: &str) -> CacheUnit {
    CacheUnit {
        kind: CacheUnitKind::VisibleText,
        alignment_key: key.into(),
        content: value.into(),
        source: SourceLocation {
            json_path: "$".into(),
            utf8_bytes: Some(0..value.len()),
            message_index: None,
            role: None,
        },
        tool_identity: None,
        hierarchy: HierarchyLocation {
            collection: ContextCollectionKind::Messages,
            parent_json_path: "$.messages".into(),
            element_index: None,
            content_block_index: None,
        },
    }
}

#[test]
fn aligns_inserted_history_instead_of_positional_zipping() {
    let medoid = vec![unit("system", "rules"), unit("user", "question")];
    let member = vec![
        unit("system", "rules"),
        unit("assistant", "old"),
        unit("user", "question"),
    ];
    let aligned = SequenceAligner::new(100).align(&medoid, &member).unwrap();
    assert_eq!(aligned.medoid_to_member, [Some(0), Some(2)]);
    assert_eq!(aligned.unmatched_member_units, [1]);
}

#[test]
fn cell_limit_is_inclusive() {
    let units = vec![unit("x", "x")];
    assert!(SequenceAligner::new(4).align(&units, &units).is_ok());
    assert!(SequenceAligner::new(3).align(&units, &units).is_err());
}

#[test]
fn does_not_align_identical_keys_across_context_hierarchy_layers() {
    let message = unit("content", "same");
    let mut block = unit("content", "same");
    block.hierarchy.collection = ContextCollectionKind::ContentBlocks;
    let aligned = SequenceAligner::new(4).align(&[message], &[block]).unwrap();
    assert_eq!(aligned.medoid_to_member, [None]);
    assert_eq!(aligned.unmatched_member_units, [0]);
}

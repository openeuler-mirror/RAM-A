//! Symbolic identity helpers hash logical structure and stable boundaries into deterministic IDs.

use sha2::{Digest, Sha256};

use crate::model::projection::{CacheUnit, ContextCollectionKind};

use super::{CoordinateKind, LogicalUnitKey};

pub(super) fn logical_unit_key(unit: &CacheUnit) -> LogicalUnitKey {
    LogicalUnitKey {
        collection: unit.hierarchy.collection.clone(),
        unit_kind: unit.kind.clone(),
        alignment_key: unit.alignment_key.clone(),
        role: unit.source.role.clone(),
        stable_identity: unit.tool_identity.clone(),
    }
}

pub(super) fn coordinate_base_id(
    unit: &CacheUnit,
    kind: &CoordinateKind,
    identity_digest: &str,
) -> String {
    digest_parts(&[
        collection_name(&unit.hierarchy.collection).as_bytes(),
        format!("{:?}", unit.kind).as_bytes(),
        unit.alignment_key.as_bytes(),
        unit.source.role.as_deref().unwrap_or("").as_bytes(),
        unit.tool_identity.as_deref().unwrap_or("").as_bytes(),
        match kind {
            CoordinateKind::Stable => b"stable",
            CoordinateKind::Slot => b"slot",
        },
        identity_digest.as_bytes(),
    ])
}

pub(super) fn kind_order(kind: &CoordinateKind) -> u8 {
    match kind {
        CoordinateKind::Stable => 0,
        CoordinateKind::Slot => 1,
    }
}

fn collection_name(collection: &ContextCollectionKind) -> &'static str {
    match collection {
        ContextCollectionKind::Request => "request",
        ContextCollectionKind::Tools => "tools",
        ContextCollectionKind::Messages => "messages",
        ContextCollectionKind::ContentBlocks => "content-blocks",
    }
}

pub(super) fn digest(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

pub(super) fn digest_parts(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
        hasher.update([0]);
    }
    hex::encode(hasher.finalize())
}

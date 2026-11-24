//! 本模块用 comparison group、逻辑边界和完整 P2 anchor chain 生成文案和值无关的身份。

use sha2::{Digest, Sha256};

use crate::model::projection::ComparisonDomain;

use super::episode::locator::Recovery;

pub(super) fn defect_id(
    domain: &ComparisonDomain,
    logical_shape: &str,
    recovery: &Recovery,
) -> String {
    let material = format!(
        "region-fp-v2\0{}\0{}\0{}\0{:?}\0{:?}\0{:?}\0{}\0{}\0{}\0{}",
        domain.time_window_key,
        domain.endpoint_key,
        domain.model,
        domain.model_deployment_key,
        domain.agent_key,
        domain.kv_namespace,
        domain.context_schema_key,
        logical_shape,
        recovery.anchor_chain_digest,
        recovery.local_shape,
    );
    format!(
        "defect-{}",
        &hex::encode(Sha256::digest(material.as_bytes()))[..24]
    )
}

pub(super) fn digest(parts: impl IntoIterator<Item = impl AsRef<[u8]>>) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_ref());
        hasher.update([0]);
    }
    hex::encode(hasher.finalize())
}

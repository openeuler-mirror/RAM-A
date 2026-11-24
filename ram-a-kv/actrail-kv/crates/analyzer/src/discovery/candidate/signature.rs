//! Bounded symmetric pair metrics compare all observable units without quadratic text edit distance.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use sha2::{Digest, Sha256};

use crate::model::projection::{CacheSequence, CacheUnit, CacheUnitKind, ContextCollectionKind};

const MAX_SHINGLES_PER_UNIT: usize = 256;
const SHINGLE_BYTES: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StructureSignature(pub String);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PairMetrics {
    pub compatibility: f64,
    pub dynamic_coverage_ratio: f64,
}

impl StructureSignature {
    pub fn of(sequence: &CacheSequence) -> Self {
        let mut tools: Vec<_> = sequence
            .units
            .iter()
            .filter_map(|unit| unit.tool_identity.as_deref())
            .collect();
        tools.sort_unstable();
        let keys = sequence
            .units
            .iter()
            .map(|unit| {
                format!(
                    "{}:{}",
                    collection_name(&unit.hierarchy.collection),
                    unit.alignment_key
                )
            })
            .collect::<Vec<_>>()
            .join("\u{1f}");
        let mut digest = Sha256::new();
        digest.update(keys.as_bytes());
        digest.update([0]);
        for tool in tools {
            digest.update(tool.as_bytes());
            digest.update([0]);
        }
        Self(hex::encode(digest.finalize()))
    }
}

/// Computes both clustering metrics once with O(units log units + sampled bytes) work.
pub fn pair_metrics(left: &CacheSequence, right: &CacheSequence) -> PairMetrics {
    if left.domain != right.domain {
        return PairMetrics {
            compatibility: 0.0,
            dynamic_coverage_ratio: 1.0,
        };
    }
    let (content, structure) = observable_similarity(left, right);
    let compatibility = match tool_jaccard(left, right) {
        Some(tools) => 0.20 * structure + 0.15 * tools + 0.65 * content,
        None => 0.25 * structure + 0.75 * content,
    };
    PairMetrics {
        compatibility,
        dynamic_coverage_ratio: 1.0 - content,
    }
}

pub fn compatibility(left: &CacheSequence, right: &CacheSequence) -> f64 {
    pair_metrics(left, right).compatibility
}

pub fn dynamic_coverage_ratio(left: &CacheSequence, right: &CacheSequence) -> f64 {
    pair_metrics(left, right).dynamic_coverage_ratio
}

/// Computes a deterministic bounded approximation for model-visible UTF-8 text similarity.
pub fn bounded_text_similarity(left: &str, right: &str) -> f64 {
    bounded_byte_similarity(left.as_bytes(), right.as_bytes())
}

type UnitKey<'a> = (
    ContextCollectionKind,
    CacheUnitKind,
    &'a str,
    Option<&'a str>,
);

fn observable_similarity(left: &CacheSequence, right: &CacheSequence) -> (f64, f64) {
    let mut left_groups: BTreeMap<UnitKey<'_>, Vec<&CacheUnit>> = BTreeMap::new();
    let mut right_groups: BTreeMap<UnitKey<'_>, Vec<&CacheUnit>> = BTreeMap::new();
    for unit in &left.units {
        left_groups.entry(unit_key(unit)).or_default().push(unit);
    }
    for unit in &right.units {
        right_groups.entry(unit_key(unit)).or_default().push(unit);
    }
    let keys: BTreeSet<_> = left_groups
        .keys()
        .cloned()
        .chain(right_groups.keys().cloned())
        .collect();
    let mut similar_bytes = 0.0;
    let mut total_bytes = 0usize;
    let mut matched_units = 0usize;
    let mut total_units = 0usize;
    for key in keys {
        let mut left_units = left_groups.remove(&key).unwrap_or_default();
        let mut right_units = right_groups.remove(&key).unwrap_or_default();
        left_units.sort_by(|a, b| a.content.cmp(&b.content));
        right_units.sort_by(|a, b| a.content.cmp(&b.content));
        total_units += left_units.len().max(right_units.len());
        let mut left_remaining = Vec::new();
        let mut right_remaining = Vec::new();
        let (mut left_index, mut right_index) = (0, 0);
        while left_index < left_units.len() && right_index < right_units.len() {
            match left_units[left_index]
                .content
                .cmp(&right_units[right_index].content)
            {
                std::cmp::Ordering::Equal => {
                    let weight = left_units[left_index].content.len().max(1);
                    total_bytes += weight;
                    similar_bytes += weight as f64;
                    matched_units += 1;
                    left_index += 1;
                    right_index += 1;
                }
                std::cmp::Ordering::Less => {
                    left_remaining.push(left_units[left_index]);
                    left_index += 1;
                }
                std::cmp::Ordering::Greater => {
                    right_remaining.push(right_units[right_index]);
                    right_index += 1;
                }
            }
        }
        left_remaining.extend_from_slice(&left_units[left_index..]);
        right_remaining.extend_from_slice(&right_units[right_index..]);
        let pairs = left_remaining.len().min(right_remaining.len());
        matched_units += pairs;
        for index in 0..pairs {
            let left_content = left_remaining[index].content.as_bytes();
            let right_content = right_remaining[index].content.as_bytes();
            let weight = left_content.len().max(right_content.len()).max(1);
            total_bytes += weight;
            similar_bytes += bounded_byte_similarity(left_content, right_content) * weight as f64;
        }
        total_bytes += left_remaining[pairs..]
            .iter()
            .map(|unit| unit.content.len().max(1))
            .sum::<usize>();
        total_bytes += right_remaining[pairs..]
            .iter()
            .map(|unit| unit.content.len().max(1))
            .sum::<usize>();
    }
    (
        similar_bytes / total_bytes.max(1) as f64,
        matched_units as f64 / total_units.max(1) as f64,
    )
}

fn unit_key(unit: &CacheUnit) -> UnitKey<'_> {
    (
        unit.hierarchy.collection.clone(),
        unit.kind.clone(),
        unit.alignment_key.as_str(),
        unit.tool_identity.as_deref(),
    )
}

fn collection_name(collection: &ContextCollectionKind) -> &'static str {
    match collection {
        ContextCollectionKind::Request => "request",
        ContextCollectionKind::Tools => "tools",
        ContextCollectionKind::Messages => "messages",
        ContextCollectionKind::ContentBlocks => "content-blocks",
    }
}

fn bounded_byte_similarity(left: &[u8], right: &[u8]) -> f64 {
    if left == right {
        return 1.0;
    }
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let mut left_counts = BTreeMap::<u64, usize>::new();
    let mut right_counts = BTreeMap::<u64, usize>::new();
    for sample in sampled_shingles(left) {
        *left_counts.entry(sample).or_default() += 1;
    }
    for sample in sampled_shingles(right) {
        *right_counts.entry(sample).or_default() += 1;
    }
    let intersection: usize = left_counts
        .iter()
        .map(|(sample, count)| count.min(right_counts.get(sample).unwrap_or(&0)))
        .sum();
    let total = left_counts.values().sum::<usize>() + right_counts.values().sum::<usize>();
    2.0 * intersection as f64 / total.max(1) as f64
}

fn sampled_shingles(bytes: &[u8]) -> Vec<u64> {
    if bytes.len() < SHINGLE_BYTES {
        return bytes.iter().map(|byte| u64::from(*byte)).collect();
    }
    let windows = bytes.len() - SHINGLE_BYTES + 1;
    let keep = windows.min(MAX_SHINGLES_PER_UNIT);
    // bottom-k min-hash：保留哈希值最小的窗口，采样与文本位置无关，
    // 局部插入/删除不会让后续窗口集体错位。
    let mut heap = BinaryHeap::new();
    for index in 0..windows {
        let hash = fnv_window(&bytes[index..index + SHINGLE_BYTES]);
        if heap.len() < keep {
            heap.push(Reverse(hash));
        } else if hash < heap.peek().expect("nonempty heap").0 {
            heap.pop();
            heap.push(Reverse(hash));
        }
    }
    let mut samples: Vec<_> = heap.into_iter().map(|item| item.0).collect();
    samples.sort_unstable();
    samples
}

fn fnv_window(window: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in window {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn tool_jaccard(left: &CacheSequence, right: &CacheSequence) -> Option<f64> {
    let left: BTreeSet<_> = left
        .units
        .iter()
        .filter_map(|unit| unit.tool_identity.as_deref())
        .collect();
    let right: BTreeSet<_> = right
        .units
        .iter()
        .filter_map(|unit| unit.tool_identity.as_deref())
        .collect();
    let union = left.union(&right).count();
    (union != 0).then(|| left.intersection(&right).count() as f64 / union as f64)
}

#[cfg(test)]
#[path = "../../../../../tests/unit/analyzer/discovery/candidate/signature.rs"]
mod tests;

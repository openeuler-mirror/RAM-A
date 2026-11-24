//! 本文件识别 gap/unmatched 插入缺失，并仅在唯一身份双射成立时声明重排。

use std::collections::{BTreeMap, BTreeSet};

use actrail_kv_artifacts::{DefectFact, DefectFactKind, OptimizationInsight};

use super::super::episode::variant::LocatedVariant;

pub(super) struct SequenceFacts {
    pub present: bool,
    pub reorder: bool,
    pub facts: Vec<DefectFact>,
    pub insights: Vec<OptimizationInsight>,
    pub certainty: f64,
}

pub(super) fn analyze(variants: &[LocatedVariant]) -> SequenceFacts {
    let present = variants.iter().any(|item| item.has_gap_or_unmatched);
    let reorder = unique_reorder(variants);
    if !present && !reorder {
        return SequenceFacts {
            present: false,
            reorder: false,
            facts: vec![],
            insights: vec![],
            certainty: 1.0,
        };
    }
    SequenceFacts {
        present: true,
        reorder,
        facts: vec![DefectFact {
            kind: if reorder {
                DefectFactKind::Reorder
            } else {
                DefectFactKind::InsertionDeletion
            },
            detail: Some(if reorder {
                "稳定且唯一的元素身份可一一对应，但顺序不同".into()
            } else {
                "成员在同一模板 region 中存在插入、缺失或位移".into()
            }),
        }],
        insights: vec![OptimizationInsight {
            summary: if reorder {
                "使用稳定且确定的元素生成顺序".into()
            } else {
                "评估将中途变化改为尾部追加".into()
            },
            detail: None,
        }],
        certainty: if reorder { 1.0 } else { 0.9 },
    }
}

fn unique_reorder(variants: &[LocatedVariant]) -> bool {
    let orders: Vec<Vec<&str>> = variants
        .iter()
        .filter_map(|variant| {
            let mut seen = BTreeSet::new();
            let order: Vec<_> = variant
                .parts
                .iter()
                .filter_map(|part| part.stable_identity.as_deref())
                .collect();
            (!order.is_empty() && order.iter().all(|identity| seen.insert(*identity)))
                .then_some(order)
        })
        .collect();
    if orders.len() != variants.len() || orders.len() < 2 {
        return false;
    }
    let sets: Vec<BTreeMap<&str, ()>> = orders
        .iter()
        .map(|order| order.iter().map(|identity| (*identity, ())).collect())
        .collect();
    sets.iter().all(|set| set == &sets[0]) && orders.iter().any(|order| order != &orders[0])
}

#[cfg(test)]
#[path = "../../../../../tests/unit/analyzer/diagnosis/facts/sequence.rs"]
mod tests;

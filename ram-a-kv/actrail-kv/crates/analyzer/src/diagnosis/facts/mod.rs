//! 本模块为同一 mismatch region 附加 content、sequence 与 representation 事实而不拆分问题。

mod content;
mod representation;
mod sequence;

use actrail_kv_artifacts::{DefectFact, MismatchPattern, OptimizationInsight};

use super::episode::variant::LocatedVariant;

pub(super) struct FactAnalysis {
    pub pattern: MismatchPattern,
    pub facts: Vec<DefectFact>,
    pub insights: Vec<OptimizationInsight>,
    pub certainty: f64,
}

pub(super) fn analyze(variants: &[LocatedVariant], fixed_variant_max: usize) -> FactAnalysis {
    let sequence = sequence::analyze(variants);
    let mut content = content::analyze(variants, fixed_variant_max);
    // 同一组唯一身份仅发生排列时，序列指纹变化是重排的结果，不再重复描述为内容漂移。
    if sequence.reorder {
        content.present = false;
        content.facts.clear();
        content.insights.clear();
    }
    let representation = representation::analyze(variants);
    let pattern = match (content.present, sequence.present, sequence.reorder) {
        (true, true, _) => MismatchPattern::Mixed,
        (false, true, true) => MismatchPattern::Reorder,
        (false, true, false) => MismatchPattern::InsertionDeletion,
        _ => MismatchPattern::ValueMismatch,
    };
    let mut facts = Vec::new();
    facts.extend(content.facts);
    facts.extend(sequence.facts);
    facts.extend(representation.facts);
    facts.sort_by(|left, right| {
        format!("{:?}", left.kind)
            .cmp(&format!("{:?}", right.kind))
            .then_with(|| left.detail.cmp(&right.detail))
    });
    facts.dedup();
    let mut insights = Vec::new();
    insights.extend(content.insights);
    insights.extend(sequence.insights);
    insights.extend(representation.insights);
    insights.sort_by(|left, right| {
        left.summary
            .cmp(&right.summary)
            .then_with(|| left.detail.cmp(&right.detail))
    });
    insights.dedup();
    FactAnalysis {
        pattern,
        facts,
        insights,
        certainty: content
            .certainty
            .min(sequence.certainty)
            .min(representation.certainty),
    }
}

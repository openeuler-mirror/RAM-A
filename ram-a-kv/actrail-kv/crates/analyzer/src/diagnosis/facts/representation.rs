//! 本文件仅在全部非空 X 文本可解析且 JSON AST 严格相等时附加等价表示事实。

use actrail_kv_artifacts::{DefectFact, DefectFactKind, OptimizationInsight};

use super::super::episode::variant::LocatedVariant;

pub(super) struct RepresentationFacts {
    pub facts: Vec<DefectFact>,
    pub insights: Vec<OptimizationInsight>,
    pub certainty: f64,
}

pub(super) fn analyze(variants: &[LocatedVariant]) -> RepresentationFacts {
    let texts: Vec<_> = variants
        .iter()
        .filter_map(|variant| (!variant.text.is_empty()).then_some(&variant.text))
        .collect();
    let parsed: Option<Vec<(String, serde_json::Value)>> =
        texts.iter().map(|text| loose_json(text)).collect();
    let equivalent = parsed.as_ref().is_some_and(|values| {
        values.len() >= 2
            && values
                .iter()
                .all(|(label, value)| label == &values[0].0 && value == &values[0].1)
            && texts.iter().any(|text| text.as_str() != texts[0].as_str())
    });
    if !equivalent {
        return RepresentationFacts {
            facts: vec![],
            insights: vec![],
            certainty: 1.0,
        };
    }
    RepresentationFacts {
        facts: vec![DefectFact {
            kind: DefectFactKind::StructuredDataEquivalent,
            detail: Some("模型可见 JSON 的语法树严格相等，但原始文本表示不同".into()),
        }],
        insights: vec![OptimizationInsight {
            summary: "使用确定的模型可见序列化格式".into(),
            detail: Some("固定对象键序、空白、换行与转义策略".into()),
        }],
        certainty: 1.0,
    }
}

/// 先整体解析；失败时剥离相同的前导标签后解析（标签必须完全一致，避免误判）。
fn loose_json(text: &str) -> Option<(String, serde_json::Value)> {
    if let Ok(value) = serde_json::from_str(text) {
        return Some((String::new(), value));
    }
    let start = text.find(['{', '['])?;
    let (label, rest) = text.split_at(start);
    if label.trim().is_empty() {
        return None;
    }
    let value = serde_json::from_str(rest).ok()?;
    Some((label.to_owned(), value))
}

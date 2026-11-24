//! 本文件验收结构缺陷识别、模板事实和排序。

use super::*;

#[test]
fn detects_early_variable_content_without_value_shape_rules() {
    let tail = stable("shared-after-volatile");
    let payloads = ["青山", "ocean account", "Δοκιμή", "客户七"]
        .into_iter()
        .map(|value| {
            chat(vec![
                json!({"role":"system","content":value}),
                json!({"role":"system","content":tail}),
            ])
        })
        .collect();
    let result = analyze(payloads);
    assert!(has_fact(&result, DefectFactKind::ContentVariation));
}

#[test]
fn detects_inline_dynamic_slot_with_unstructured_values() {
    let suffix = stable("common-inline-suffix");
    let contents: Vec<_> = ["青山", "ocean", "Δοκιμή", "客户七"]
        .into_iter()
        .map(|value| format!("固定开头::{value}::{suffix}"))
        .collect();
    let payloads = contents
        .iter()
        .map(|content| chat(vec![json!({"role":"system","content":content})]))
        .collect();
    let result = analyze(payloads);
    assert!(has_fact(&result, DefectFactKind::ContentVariation));
    let defect = result.defects.first().expect("defect");
    let offset = defect.mismatch.variants[0].representative.sources[0]
        .byte_start
        .expect("byte offset");
    assert!(contents
        .iter()
        .all(|content| offset <= content.len() && content.is_char_boundary(offset)));
}

#[test]
fn detects_dynamic_block_inserted_before_stable_block() {
    let stable_block = stable("fixed-few-shot");
    let mut payloads = vec![chat(vec![json!({"role":"system","content":stable_block})])];
    for value in ["retrieval-A", "another knowledge", "检索片段三"] {
        payloads.push(chat(vec![
            json!({"role":"system","content":value}),
            json!({"role":"system","content":stable_block}),
        ]));
    }
    let result = analyze(payloads);
    assert!(has_fact(&result, DefectFactKind::InsertionDeletion));
    assert!(has_fact(&result, DefectFactKind::ContentVariation));
    assert_eq!(result.defects[0].mismatch.pattern, MismatchPattern::Mixed);
}

#[test]
fn detects_unique_tool_reordering() {
    let tool_a = json!({"type":"function","function":{"name":"lookup","description":stable("lookup"),"parameters":{"type":"object"}}});
    let tool_b = json!({"type":"function","function":{"name":"calculate","description":stable("calculate"),"parameters":{"type":"object"}}});
    let payloads = [false, false, true, true]
        .into_iter()
        .map(|reversed| {
            let tools = if reversed {
                vec![tool_b.clone(), tool_a.clone()]
            } else {
                vec![tool_a.clone(), tool_b.clone()]
            };
            json!({"model":"model-a","tools":tools,"messages":[{"role":"system","content":stable("after-tools")} ]})
        })
        .collect();
    let result = analyze(payloads);
    assert!(
        has_fact(&result, DefectFactKind::Reorder),
        "result={result:#?}"
    );
    assert_eq!(result.defects[0].mismatch.pattern, MismatchPattern::Reorder);
}

#[test]
fn detects_tool_definition_and_prompt_version_drift() {
    let descriptions = [
        "Search internal docs.",
        "Search internal docs.",
        "Search internal documentation!",
        "Search internal documentation!",
    ];
    let tool_payloads = descriptions
        .into_iter()
        .map(|description| json!({
            "model":"model-a",
            "tools":[{"type":"function","function":{"name":"lookup","description":description,"parameters":{"type":"object","properties":{"query":{"type":"string"}}}}}],
            "messages":[{"role":"system","content":stable("after-definition")}]
        }))
        .collect();
    let tool_result = analyze(tool_payloads);
    assert!(has_content_fact(&tool_result), "result={tool_result:#?}");

    let prompt_payloads = [
        "Follow policy A. ",
        "Follow policy A. ",
        "Follow policy A! ",
        "Follow policy A! ",
    ]
    .into_iter()
    .map(|prefix| {
        chat(vec![json!({
            "role":"system",
            "content":format!("{prefix}{}", stable("prompt-tail"))
        })])
    })
    .collect();
    assert!(has_content_fact(&analyze(prompt_payloads)));
}

#[test]
fn detects_non_append_history_and_visible_json_formatting() {
    let old = stable("old-history");
    let baseline = chat(vec![
        json!({"role":"system","content":"fixed"}),
        json!({"role":"user","content":old}),
    ]);
    let mut histories = vec![baseline];
    for value in ["new-a", "新增消息乙", "another-new"] {
        histories.push(chat(vec![
            json!({"role":"system","content":"fixed"}),
            json!({"role":"user","content":value}),
            json!({"role":"user","content":old}),
        ]));
    }
    let history_result = analyze(histories);
    assert!(has_fact(&history_result, DefectFactKind::InsertionDeletion));

    let payloads = [
        "{\"alpha\":1,\"beta\":2}",
        "{ \"beta\": 2, \"alpha\": 1 }",
        "{\n  \"alpha\": 1, \"beta\": 2\n}",
        "{\"beta\":2,\"alpha\":1}",
    ]
    .into_iter()
    .map(|visible| {
        chat(vec![
            json!({"role":"system","content":visible}),
            json!({"role":"system","content":stable("after-json")}),
        ])
    })
    .collect();
    assert!(has_fact(
        &analyze(payloads),
        DefectFactKind::StructuredDataEquivalent
    ));
}

#[test]
fn mixed_region_with_content_and_missing_members_is_one_mixed_defect() {
    let tail = stable("shared-after-block");
    let mut payloads = vec![chat(vec![json!({"role":"system","content":tail})])];
    for block in ["retrieval-A", "检索片段乙", "knowledge-C"] {
        payloads.push(chat(vec![
            json!({"role":"system","content":block}),
            json!({"role":"system","content":tail}),
        ]));
    }
    let result = analyze(payloads);
    assert_eq!(result.defects.len(), 1, "result={result:#?}");
    assert_eq!(result.defects[0].mismatch.pattern, MismatchPattern::Mixed);
    assert!(has_fact(&result, DefectFactKind::ContentVariation));
    assert!(has_fact(&result, DefectFactKind::InsertionDeletion));
}

#[test]
fn two_fixed_versions_are_reported_as_fixed_variants() {
    let tail = stable("tail-after-version");
    let payloads: Vec<Value> = [
        "follow policy A.",
        "follow policy A.",
        "follow policy B.",
        "follow policy B.",
    ]
    .into_iter()
    .map(|version| {
        chat(vec![json!({
            "role":"system",
            "content":format!("{version}\n{tail}")
        })])
    })
    .collect();
    let result = analyze(payloads);
    assert!(has_fact(&result, DefectFactKind::FixedVariants));
    assert!(!has_fact(&result, DefectFactKind::ContentVariation));
    assert_eq!(
        result.defects[0].mismatch.pattern,
        MismatchPattern::ValueMismatch
    );
}

#[test]
fn prefix_view_exposes_p_x_chain_and_variants() {
    let tail = stable("prefix-view-tail");
    let mut payloads = Vec::new();
    for version in ["1", "2", "3"] {
        for _ in 0..2 {
            payloads.push(chat(vec![json!({
                "role": "system",
                "content": format!("version:{version}\n{tail}"),
            })]));
        }
    }
    let result = analyze(payloads);
    let template = result
        .templates
        .iter()
        .find(|template| template.prefix_view.is_some())
        .unwrap_or_else(|| panic!("no template with prefix_view: {result:#?}"));
    let view = template.prefix_view.as_ref().expect("prefix view present");
    assert!(
        view.nodes
            .iter()
            .any(|node| matches!(node.kind, actrail_kv_artifacts::PrefixNodeKind::Dynamic)),
        "view={view:#?}"
    );
    assert!(
        view.nodes
            .iter()
            .any(|node| matches!(node.kind, actrail_kv_artifacts::PrefixNodeKind::Stable)),
        "view={view:#?}"
    );
    let dynamic = view
        .nodes
        .iter()
        .find(|node| node.variants.is_some())
        .expect("dynamic node with variants");
    assert_eq!(dynamic.variants.as_ref().unwrap().len(), 3);
    assert!(
        dynamic.sequence_child.is_some() || !dynamic.internal_children.is_empty(),
        "dynamic node should have a sequence child or internal children"
    );
    assert!(
        !result.defects.is_empty(),
        "fixed policy versions should produce a defect: {result:#?}"
    );
    assert!(
        view.nodes.iter().any(|node| !node.defect_ids.is_empty()),
        "prefix view should attach defect ids: {view:#?}"
    );
}

#[test]
fn fixed_variant_threshold_is_configurable() {
    let tail = stable("tail-after-version");
    let payloads: Vec<Value> = [
        "follow policy A.",
        "follow policy A.",
        "follow policy B.",
        "follow policy B.",
    ]
    .into_iter()
    .map(|version| {
        chat(vec![json!({
            "role":"system",
            "content":format!("{version}\n{tail}")
        })])
    })
    .collect();
    let strict = analyze_with_options(
        payloads.clone(),
        None,
        AnalysisOptions {
            fixed_variant_max: 1,
            ..AnalysisOptions::default()
        },
    );
    assert!(has_fact(&strict, DefectFactKind::ContentVariation));
    assert!(!has_fact(&strict, DefectFactKind::FixedVariants));
}

#[test]
fn one_template_with_two_dynamic_regions_reports_two_defects() {
    let mid = stable("first-region-tail");
    let tail = stable("second-region-tail");
    let workspaces = ["north", "south", "east", "west"];
    let modes = ["a", "b", "c", "d"];
    let payloads = workspaces
        .iter()
        .zip(modes)
        .map(|(workspace, mode)| {
            chat(vec![json!({
                "role":"system",
                "content":format!("You are an agent.\nWorkspace: {workspace}\n{mid}\nMode: {mode}\n{tail}")
            })])
        })
        .collect();
    let result = analyze(payloads);
    assert!(result.defects.len() >= 2, "result={result:#?}");
    assert!(result
        .defects
        .iter()
        .all(|defect| defect.mismatch.pattern == MismatchPattern::ValueMismatch));
    let distinct_regions: std::collections::BTreeSet<_> = result
        .defects
        .iter()
        .map(|defect| defect.recovered_stable.excerpt.clone())
        .collect();
    assert_eq!(distinct_regions.len(), result.defects.len());
}

#[test]
fn json_array_reorder_is_not_structured_equivalence() {
    let tail = stable("tail-after-array");
    let payloads = ["[1,2]", "[2,1]", "[1,2]", "[2,1]"]
        .into_iter()
        .map(|visible| {
            chat(vec![
                json!({"role":"system","content":visible}),
                json!({"role":"system","content":tail}),
            ])
        })
        .collect();
    let result = analyze(payloads);
    assert!(has_content_fact(&result));
    assert!(!has_fact(&result, DefectFactKind::StructuredDataEquivalent));
}

#[test]
fn json_value_change_is_not_structured_equivalence() {
    let tail = stable("tail-after-object");
    let payloads = [r#"{"a":1}"#, r#"{"a":2}"#, r#"{"a":1}"#, r#"{"a":2}"#]
        .into_iter()
        .map(|visible| {
            chat(vec![
                json!({"role":"system","content":visible}),
                json!({"role":"system","content":tail}),
            ])
        })
        .collect();
    let result = analyze(payloads);
    assert!(has_content_fact(&result));
    assert!(!has_fact(&result, DefectFactKind::StructuredDataEquivalent));
}

#[test]
fn labeled_json_equivalent_representations_are_detected() {
    let tail = stable("tail-after-labeled-json");
    let payloads = [
        "Config JSON: {\"alpha\":1,\"beta\":2}",
        "Config JSON: { \"beta\": 2, \"alpha\": 1 }",
        "Config JSON: {\n  \"alpha\": 1, \"beta\": 2\n}",
        "Config JSON: {\"beta\":2,\"alpha\":1}",
    ]
    .into_iter()
    .map(|visible| {
        chat(vec![
            json!({"role":"system","content":visible}),
            json!({"role":"system","content":tail}),
        ])
    })
    .collect();
    let result = analyze(payloads);
    assert!(has_fact(&result, DefectFactKind::StructuredDataEquivalent));
}

#[test]
fn suppresses_dynamic_suffix_non_context_distinct_template_and_cross_domain() {
    let suffix_only = ["question a", "问题乙", "third request", "delta"]
        .into_iter()
        .map(|question| {
            chat(vec![
                json!({"role":"system","content":stable("same-prefix")}),
                json!({"role":"user","content":question}),
            ])
        })
        .collect();
    assert!(analyze(suffix_only).defects.is_empty());

    let non_context = [0.1, 0.2, 0.3, 0.4]
        .into_iter()
        .map(|temperature| json!({"model":"model-a","temperature":temperature,"messages":[{"role":"system","content":stable("identical")}]}))
        .collect();
    assert!(analyze(non_context).defects.is_empty());

    let distinct = [
        "translate text",
        "review code",
        "answer legal questions",
        "compose music",
    ]
    .into_iter()
    .map(|role| chat(vec![json!({"role":"system","content":role})]))
    .collect();
    assert!(analyze(distinct).defects.is_empty());

    let cross_model = ["a", "b", "c", "d"]
        .into_iter()
        .map(|model| json!({"model":model,"messages":[{"role":"system","content":format!("variable {model} {}",stable("tail"))}]}))
        .collect();
    assert!(analyze(cross_model).defects.is_empty());
}

#[test]
fn defects_and_ranking_are_input_permutation_invariant() {
    let tail = stable("permutation-tail");
    let mut payloads: Vec<_> = ["north", "south", "east", "west"]
        .into_iter()
        .map(|value| {
            chat(vec![
                json!({"role":"system","content":value}),
                json!({"role":"system","content":tail}),
            ])
        })
        .collect();
    let first = analyze(payloads.clone());
    payloads.reverse();
    let second = analyze(payloads);
    assert_eq!(first.templates, second.templates);
    assert_eq!(first.defects, second.defects);
    assert_eq!(first.top_k, second.top_k);
}

#[test]
fn mixed_corpus_ranks_aggregated_roots_without_pair_amplification() {
    fn group(label: &str, member_count: usize, stable_bytes: usize) -> Vec<Value> {
        (0..member_count)
            .map(|index| {
                chat(vec![
                    json!({"role":"system","content":format!("{label}-variant-{index}")}),
                    json!({"role":"system","content":label.repeat(stable_bytes / label.len())}),
                ])
            })
            .collect()
    }

    let mut payloads = group("ALPHA", 6, 600);
    payloads.extend(group("BRAVO", 5, 360));
    payloads.extend(group("CHARLIE", 4, 180));
    let result = analyze(payloads);

    assert_eq!(result.defects.len(), 3, "defects={:?}", result.defects);
    assert_eq!(result.top_k.len(), 3);
    let ranked: Vec<_> = result
        .top_k
        .iter()
        .map(|id| {
            result
                .defects
                .iter()
                .find(|defect| &defect.id == id)
                .expect("ranked defect exists")
        })
        .collect();
    assert!(ranked[0].score.score > ranked[1].score.score);
    assert!(ranked[1].score.score > ranked[2].score.score);
    assert_eq!(ranked[0].affected_count, 5);
    assert_eq!(ranked[1].affected_count, 4);
    assert_eq!(ranked[2].affected_count, 3);
}

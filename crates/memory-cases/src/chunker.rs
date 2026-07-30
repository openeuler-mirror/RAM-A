use std::sync::Arc;

use anyhow::Result;

use crate::parser::{ParseNode, ParseNodeType, ParseResult, ParseTopology};
use crate::token_counter::TokenCounter;

#[derive(Clone)]
pub struct ChunkerConfig {
    pub max_tokens: usize,
    pub token_counter: Arc<dyn TokenCounter>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChunkPart {
    pub content: String,
    pub chunk_type: ParseNodeType,
    pub parse_topology: ParseTopology,
    pub token_count: usize,
    pub source_node_indices: Vec<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChunkNode {
    content: String,
    node_type: ParseNodeType,
    is_need_space: bool,
    is_need_newline: bool,
    source_node_indices: Vec<usize>,
}

impl ChunkNode {
    fn text(content: impl Into<String>, source_node_indices: Vec<usize>) -> Self {
        Self {
            content: content.into(),
            node_type: ParseNodeType::Text,
            is_need_space: false,
            is_need_newline: true,
            source_node_indices,
        }
    }
}

impl From<ParseNode> for ChunkNode {
    fn from(node: ParseNode) -> Self {
        Self {
            content: node.content,
            node_type: node.node_type,
            is_need_space: node.is_need_space,
            is_need_newline: node.is_need_newline,
            source_node_indices: vec![node.index],
        }
    }
}

pub fn chunk_parse_result(
    parse_result: ParseResult,
    config: ChunkerConfig,
) -> Result<Vec<ChunkPart>> {
    let max_tokens = config.max_tokens.max(1);
    let token_counter = config.token_counter.as_ref();
    let topology = parse_result.topology;
    let nodes = parse_result
        .nodes
        .into_iter()
        .map(ChunkNode::from)
        .collect::<Vec<_>>();
    let nodes = process_parse_nodes(nodes);
    let nodes = merge_adjacent_text_nodes(nodes, max_tokens, token_counter)?;
    let nodes = split_text_nodes(nodes, max_tokens, token_counter)?;

    let mut parts = Vec::new();
    for node in nodes {
        let content = node.content.trim().to_string();
        if !content.is_empty() {
            parts.push(ChunkPart {
                token_count: token_counter.count_tokens(&content)?,
                content,
                chunk_type: node.node_type,
                parse_topology: topology,
                source_node_indices: node.source_node_indices,
            });
        }
    }
    Ok(parts)
}

fn process_parse_nodes(nodes: Vec<ChunkNode>) -> Vec<ChunkNode> {
    nodes
        .into_iter()
        .filter_map(|mut node| {
            node.content = node.content.trim().to_string();
            if node.content.is_empty() {
                None
            } else {
                Some(node)
            }
        })
        .collect()
}

fn merge_adjacent_text_nodes(
    nodes: Vec<ChunkNode>,
    max_tokens: usize,
    token_counter: &dyn TokenCounter,
) -> Result<Vec<ChunkNode>> {
    let mut merged = Vec::<ChunkNode>::new();
    for node in nodes {
        if node.node_type != ParseNodeType::Text {
            merged.push(node);
            continue;
        }

        let Some(last) = merged.last_mut() else {
            merged.push(node);
            continue;
        };
        if last.node_type != ParseNodeType::Text {
            merged.push(node);
            continue;
        }

        let candidate = join_with_node_spacing(&last.content, &node);
        if token_counter.count_tokens(&candidate)? <= max_tokens {
            last.content = candidate;
            extend_unique(&mut last.source_node_indices, &node.source_node_indices);
        } else {
            merged.push(node);
        }
    }
    Ok(merged)
}

fn split_text_nodes(
    nodes: Vec<ChunkNode>,
    max_tokens: usize,
    token_counter: &dyn TokenCounter,
) -> Result<Vec<ChunkNode>> {
    let mut chunks = Vec::new();
    let mut pending_text: Option<ChunkNode> = None;

    for node in nodes {
        if node.node_type != ParseNodeType::Text {
            flush_pending_text(&mut chunks, &mut pending_text);
            chunks.push(node);
            continue;
        }

        let sentences =
            split_node_into_budgeted_sentences(&node.content, max_tokens, token_counter)?;
        for sentence in sentences {
            let sentence_node = ChunkNode::text(sentence, node.source_node_indices.clone());
            let Some(pending) = pending_text.as_mut() else {
                pending_text = Some(sentence_node);
                continue;
            };

            let candidate = join_budgeted_text_segments(&pending.content, &sentence_node.content);
            if token_counter.count_tokens(&candidate)? > max_tokens {
                flush_pending_text(&mut chunks, &mut pending_text);
                pending_text = Some(sentence_node);
            } else {
                pending.content = candidate;
                extend_unique(
                    &mut pending.source_node_indices,
                    &sentence_node.source_node_indices,
                );
            }
        }
    }

    flush_pending_text(&mut chunks, &mut pending_text);
    Ok(chunks)
}

fn split_node_into_budgeted_sentences(
    text: &str,
    max_tokens: usize,
    token_counter: &dyn TokenCounter,
) -> Result<Vec<String>> {
    let sentences = split_sentences(text);
    let sentences = if sentences.is_empty() {
        vec![text.trim().to_string()]
    } else {
        sentences
    };

    let mut budgeted = Vec::new();
    for sentence in sentences {
        if token_counter.count_tokens(&sentence)? <= max_tokens {
            budgeted.push(sentence);
            continue;
        }

        let mut rest = sentence.as_str();
        while !rest.trim().is_empty() {
            let prefix = take_token_prefix(rest, max_tokens, token_counter)?;
            if prefix.is_empty() {
                break;
            }
            let prefix_len = prefix.len();
            budgeted.push(prefix);
            rest = rest[prefix_len..].trim_start();
        }
    }
    Ok(budgeted)
}

fn flush_pending_text(chunks: &mut Vec<ChunkNode>, pending_text: &mut Option<ChunkNode>) {
    let Some(mut pending) = pending_text.take() else {
        return;
    };
    pending.content = pending.content.trim().to_string();
    if !pending.content.is_empty() {
        chunks.push(pending);
    }
}

fn join_budgeted_text_segments(left: &str, right: &str) -> String {
    let left = left.trim_end();
    let right = right.trim_start();
    if left.is_empty() {
        return right.to_string();
    }
    if right.is_empty() {
        return left.to_string();
    }

    if left
        .chars()
        .last()
        .map(is_sentence_boundary_char)
        .unwrap_or(false)
    {
        format!("{left}{right}")
    } else {
        format!("{left}\n{right}")
    }
}

fn is_sentence_boundary_char(character: char) -> bool {
    matches!(
        character,
        '。' | '！' | '？' | '!' | '?' | '；' | ';' | '.'
    )
}

fn join_with_node_spacing(left: &str, right: &ChunkNode) -> String {
    if right.is_need_newline {
        format!("{}\n{}", left.trim_end(), right.content.trim_start())
    } else if right.is_need_space {
        format!("{} {}", left.trim_end(), right.content.trim_start())
    } else {
        format!("{}{}", left, right.content)
    }
}

fn extend_unique(target: &mut Vec<usize>, values: &[usize]) {
    for value in values {
        if !target.contains(value) {
            target.push(*value);
        }
    }
}

fn split_sentences(text: &str) -> Vec<String> {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return Vec::new();
    }

    let mut sentences = Vec::new();
    let mut start = 0;
    for index in 0..chars.len() {
        if !is_sentence_boundary(&chars, index) {
            continue;
        }

        let sentence = chars[start..=index]
            .iter()
            .collect::<String>()
            .trim()
            .to_string();
        if !sentence.is_empty() {
            sentences.push(sentence);
        }
        start = index + 1;
    }

    if start < chars.len() {
        let sentence = chars[start..]
            .iter()
            .collect::<String>()
            .trim()
            .to_string();
        if !sentence.is_empty() {
            sentences.push(sentence);
        }
    }

    sentences
}

fn is_sentence_boundary(chars: &[char], index: usize) -> bool {
    match chars[index] {
        '。' | '！' | '？' | '!' | '?' | '；' | ';' => true,
        '.' => {
            let followed_by_space_or_end = chars
                .get(index + 1)
                .map(|next| next.is_whitespace())
                .unwrap_or(true);
            followed_by_space_or_end && !ends_with_protected_period(chars, index)
        }
        _ => false,
    }
}

fn ends_with_protected_period(chars: &[char], index: usize) -> bool {
    let start = index.saturating_sub(12);
    let prefix = chars[start..=index].iter().collect::<String>();
    const PROTECTED: &[&str] = &[
        "e.g.", "i.e.", "U.S.", "U.K.", "A.M.", "P.M.", "a.m.", "p.m.", "Inc.", "Ltd.",
        "No.", "vs.", "approx.", "Dr.", "Mr.", "Ms.", "Prof.",
    ];
    PROTECTED.iter().any(|item| prefix.ends_with(item))
}

fn take_token_prefix(
    content: &str,
    max_tokens: usize,
    token_counter: &dyn TokenCounter,
) -> Result<String> {
    if token_counter.count_tokens(content)? <= max_tokens {
        return Ok(content.trim().to_string());
    }

    let mut boundaries = Vec::new();
    boundaries.push(0);
    boundaries.extend(content.char_indices().skip(1).map(|(index, _)| index));
    boundaries.push(content.len());

    let mut left = 0usize;
    let mut right = boundaries.len() - 1;
    while left + 1 < right {
        let mid = (left + right) / 2;
        if token_counter.count_tokens(&content[..boundaries[mid]])? <= max_tokens {
            left = mid;
        } else {
            right = mid;
        }
    }

    let prefix = content[..boundaries[left]].trim();
    if !prefix.is_empty() {
        return Ok(prefix.to_string());
    }

    Ok(content
        .chars()
        .next()
        .map(|ch| ch.to_string())
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::ParserEngine;
    use crate::token_counter::TestTokenCounter;
    use std::io::Cursor;

    fn contents(parts: Vec<ChunkPart>) -> Vec<String> {
        parts.into_iter().map(|part| part.content).collect()
    }

    fn chunk_markdown_str(text: &str, config: ChunkerConfig) -> Vec<String> {
        let parse_result = ParserEngine::parse_markdown_reader(Cursor::new(text)).unwrap();
        contents(chunk_parse_result(parse_result, config).unwrap())
    }

    fn chunk_text_str(text: &str, config: ChunkerConfig) -> Vec<String> {
        let parse_result = ParserEngine::parse_text_reader(Cursor::new(text)).unwrap();
        contents(chunk_parse_result(parse_result, config).unwrap())
    }

    fn config(max_tokens: usize) -> ChunkerConfig {
        ChunkerConfig {
            max_tokens,
            token_counter: Arc::new(TestTokenCounter),
        }
    }

    #[test]
    fn text_reader_splits_one_large_text_node_by_sentence_budget() {
        let chunks = chunk_text_str("第一句。第二句。第三句。", config(4));

        assert_eq!(chunks, vec!["第一句。", "第二句。", "第三句。"]);
    }

    #[test]
    fn text_reader_splits_long_sentence_by_token_prefix() {
        let chunks = chunk_text_str("abcdefghij", config(2));

        assert_eq!(chunks, vec!["abcdefgh", "ij"]);
    }

    #[test]
    fn text_reader_merges_short_sentences_until_budget() {
        let chunks = chunk_text_str("甲。乙。丙。", config(4));

        assert_eq!(chunks, vec!["甲。乙。", "丙。"]);
    }

    #[test]
    fn budgeted_text_merge_keeps_boundary_after_prefix_split() {
        assert_eq!(
            join_budgeted_text_segments("故障可稳定复现于101核", "问题根因\n\n栈空间不足。"),
            "故障可稳定复现于101核\n问题根因\n\n栈空间不足。"
        );
        assert_eq!(join_budgeted_text_segments("甲。", "乙。"), "甲。乙。");
    }

    #[test]
    fn markdown_injects_heading_path_into_leaf_chunks() {
        let chunks = chunk_markdown_str("# A\n\nalpha\n\n## B\n\nbeta", config(64));

        assert_eq!(chunks, vec!["A\n\nalpha\nA > B\n\nbeta"]);
    }

    #[test]
    fn markdown_table_rows_become_independent_nodes() {
        let chunks = chunk_markdown_str("# T\n\n| key | value |\n| --- | --- |\n| a | b |", config(64));

        assert_eq!(chunks, vec!["T\n\nkey | value", "T\n\na | b"]);
    }

    #[test]
    fn markdown_code_nodes_are_not_merged_with_text() {
        let chunks = chunk_markdown_str(
            "# T\n\npara\n\n```rust\nfn main() {}\n```\n\nafter",
            config(64),
        );

        assert_eq!(
            chunks,
            vec!["T\n\npara", "T\n\nfn main() {}", "T\n\nafter"]
        );
    }

    #[test]
    fn chunk_parts_keep_parser_node_metadata() {
        let parse_result = ParserEngine::parse_markdown_reader(Cursor::new(
            "# T\n\npara\n\n| key | value |\n| --- | --- |",
        ))
        .unwrap();
        let chunks = chunk_parse_result(parse_result, config(64));

        let chunks = chunks.unwrap();
        assert_eq!(chunks[0].chunk_type, ParseNodeType::Text);
        assert_eq!(chunks[0].parse_topology, ParseTopology::List);
        assert_eq!(chunks[0].source_node_indices, vec![0]);
        assert_eq!(chunks[1].chunk_type, ParseNodeType::Table);
        assert_eq!(chunks[1].source_node_indices, vec![1]);
    }

    #[test]
    fn clamps_zero_max_tokens() {
        let chunks = chunk_text_str("ab", config(0));

        assert_eq!(chunks, vec!["ab"]);
    }
}

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, to_value};

use crate::canonical::{estimate_tokens, stable_hash};
use crate::error::{PipelineError, Result};
use crate::models::{ConversationEpisode, ExtractionWindow, MessageRef, NormalizedMessage};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WindowConfig {
    pub max_candidate_tokens: usize,
    pub max_window_tokens: usize,
    pub context_before_messages: usize,
    pub context_after_messages: usize,
    pub tokenizer_name: String,
    pub tokenizer_version: String,
    pub version: String,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            max_candidate_tokens: 320,
            max_window_tokens: 640,
            context_before_messages: 2,
            context_after_messages: 0,
            tokenizer_name: "heuristic".into(),
            tokenizer_version: "heuristic_v1".into(),
            version: "window_v1".into(),
        }
    }
}

pub fn build_windows(
    episodes: &[ConversationEpisode],
    messages_by_id: &HashMap<String, NormalizedMessage>,
    config: &WindowConfig,
) -> Result<Vec<ExtractionWindow>> {
    validate_config(config)?;
    let mut windows = Vec::new();
    for episode in episodes {
        let mut all_refs = Vec::new();
        let mut eligible_ref_indices = Vec::new();
        for id in &episode.message_ids {
            let message = messages_by_id.get(id).ok_or_else(|| {
                PipelineError::InvalidInput(format!("episode references unknown message: {id}"))
            })?;
            for reference in slice_message(message, config.max_candidate_tokens) {
                if message.candidate_eligible {
                    eligible_ref_indices.push(all_refs.len());
                }
                all_refs.push(reference);
            }
        }
        let eligible_refs = eligible_ref_indices
            .iter()
            .map(|index| all_refs[*index].clone())
            .collect::<Vec<_>>();
        for (start, end) in pack_ranges(&eligible_refs, config.max_candidate_tokens) {
            let candidate = eligible_refs[start..end].to_vec();
            let candidate_indices = &eligible_ref_indices[start..end];
            let all_start = eligible_ref_indices[start];
            let all_end = eligible_ref_indices[end - 1] + 1;
            let mut before = select_before(&all_refs[..all_start], config.context_before_messages);
            before.extend(
                (all_start..all_end)
                    .filter(|index| candidate_indices.binary_search(index).is_err())
                    .map(|index| all_refs[index].clone()),
            );
            let after = select_after(&all_refs[all_end..], config.context_after_messages);
            let (before, after) = trim_context(before, &candidate, after, config.max_window_tokens);
            windows.push(make_window(episode, candidate, before, after, config));
        }
    }
    Ok(windows)
}

pub fn render_window(
    window: &ExtractionWindow,
    messages_by_id: &HashMap<String, NormalizedMessage>,
) -> Result<String> {
    let render = |reference: &MessageRef| -> Result<String> {
        let message = messages_by_id.get(&reference.message_id).ok_or_else(|| {
            PipelineError::InvalidInput(format!(
                "window references unknown message: {}",
                reference.message_id
            ))
        })?;
        Ok(format!(
            "[message_id={} role={} speaker={} time={} span={}:{}]\n{}",
            message.id,
            message.role,
            if message.speaker.is_empty() {
                "-"
            } else {
                &message.speaker
            },
            if message.timestamp.is_empty() {
                "-"
            } else {
                &message.timestamp
            },
            reference.start_char,
            reference.end_char,
            reference.text
        ))
    };
    let mut lines = vec!["<context>".to_owned()];
    for reference in window
        .context_before_refs
        .iter()
        .chain(window.context_after_refs.iter())
    {
        lines.push(render(reference)?);
    }
    lines.extend(["</context>".into(), String::new(), "<candidate>".into()]);
    for reference in &window.candidate_refs {
        lines.push(render(reference)?);
    }
    lines.push("</candidate>".into());
    Ok(lines.join("\n"))
}

fn validate_config(config: &WindowConfig) -> Result<()> {
    if config.max_candidate_tokens == 0 {
        return Err(PipelineError::InvalidInput(
            "max_candidate_tokens must be positive".into(),
        ));
    }
    if config.max_window_tokens < config.max_candidate_tokens {
        return Err(PipelineError::InvalidInput(
            "max_window_tokens must be >= max_candidate_tokens".into(),
        ));
    }
    Ok(())
}

fn slice_message(message: &NormalizedMessage, max_tokens: usize) -> Vec<MessageRef> {
    if estimate_tokens(&message.text) <= max_tokens {
        return vec![message_ref(message, 0, message.text.chars().count())];
    }
    sentence_spans(&message.text)
        .into_iter()
        .flat_map(|(start, end)| split_span(message, start, end, max_tokens))
        .collect()
}

fn sentence_spans(text: &str) -> Vec<(usize, usize)> {
    let chars = text.chars().collect::<Vec<_>>();
    let mut spans = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        let mut end = None;
        if matches!(current, '。' | '！' | '？' | '!' | '?' | '；' | ';') {
            end = Some(index + 1);
        } else if current == '.' && (index + 1 == chars.len() || chars[index + 1].is_whitespace()) {
            end = Some(if index + 1 < chars.len() {
                index + 2
            } else {
                index + 1
            });
        } else if current == '\n' {
            let mut cursor = index + 1;
            while cursor < chars.len() && chars[cursor] == '\n' {
                cursor += 1;
            }
            end = Some(cursor);
        }
        if let Some(end) = end {
            if end > start {
                spans.push((start, end));
            }
            start = end;
            index = end;
        } else {
            index += 1;
        }
    }
    if start < chars.len() {
        spans.push((start, chars.len()));
    }
    spans
}

fn split_span(
    message: &NormalizedMessage,
    start: usize,
    end: usize,
    max_tokens: usize,
) -> Vec<MessageRef> {
    if estimate_tokens(&char_slice(&message.text, start, end)) <= max_tokens {
        return vec![message_ref(message, start, end)];
    }
    let mut output = Vec::new();
    let mut piece_start = start;
    let mut cursor = start;
    while cursor < end {
        let next = cursor + 1;
        if cursor > piece_start
            && estimate_tokens(&char_slice(&message.text, piece_start, next)) > max_tokens
        {
            output.push(message_ref(message, piece_start, cursor));
            piece_start = cursor;
        }
        cursor = next;
    }
    if piece_start < end {
        output.push(message_ref(message, piece_start, end));
    }
    output
}

fn message_ref(message: &NormalizedMessage, start: usize, end: usize) -> MessageRef {
    MessageRef {
        message_id: message.id.clone(),
        start_char: start,
        end_char: end,
        text: char_slice(&message.text, start, end),
    }
}

fn char_slice(text: &str, start: usize, end: usize) -> String {
    text.chars().skip(start).take(end - start).collect()
}

fn pack_ranges(refs: &[MessageRef], max_tokens: usize) -> Vec<(usize, usize)> {
    let mut groups = Vec::new();
    let mut start = 0;
    let mut tokens = 0;
    for (index, reference) in refs.iter().enumerate() {
        let next = estimate_tokens(&reference.text);
        if index > start && tokens + next > max_tokens {
            groups.push((start, index));
            start = index;
            tokens = 0;
        }
        tokens += next;
    }
    if start < refs.len() {
        groups.push((start, refs.len()));
    }
    groups
}

fn select_before(refs: &[MessageRef], limit: usize) -> Vec<MessageRef> {
    if limit == 0 {
        return Vec::new();
    }
    let mut selected = Vec::new();
    let mut seen = Vec::<String>::new();
    for reference in refs.iter().rev() {
        if !seen.contains(&reference.message_id) {
            if seen.len() >= limit {
                break;
            }
            seen.push(reference.message_id.clone());
        }
        selected.push(reference.clone());
    }
    selected.reverse();
    selected
}

fn select_after(refs: &[MessageRef], limit: usize) -> Vec<MessageRef> {
    if limit == 0 {
        return Vec::new();
    }
    let mut selected = Vec::new();
    let mut seen = Vec::<String>::new();
    for reference in refs {
        if !seen.contains(&reference.message_id) {
            if seen.len() >= limit {
                break;
            }
            seen.push(reference.message_id.clone());
        }
        selected.push(reference.clone());
    }
    selected
}

fn trim_context(
    mut before: Vec<MessageRef>,
    candidate: &[MessageRef],
    mut after: Vec<MessageRef>,
    max_tokens: usize,
) -> (Vec<MessageRef>, Vec<MessageRef>) {
    while refs_tokens(before.iter().chain(candidate).chain(after.iter())) > max_tokens {
        if !before.is_empty() {
            before.remove(0);
        } else if !after.is_empty() {
            after.pop();
        } else {
            break;
        }
    }
    (before, after)
}

fn refs_tokens<'a>(refs: impl Iterator<Item = &'a MessageRef>) -> usize {
    refs.map(|reference| estimate_tokens(&reference.text)).sum()
}

fn make_window(
    episode: &ConversationEpisode,
    candidate: Vec<MessageRef>,
    before: Vec<MessageRef>,
    after: Vec<MessageRef>,
    config: &WindowConfig,
) -> ExtractionWindow {
    let id = format!(
        "window-{}",
        stable_hash(&[
            json!(episode.scope_id),
            json!(episode.session_id),
            to_value(&candidate).expect("serializable refs"),
            to_value(&before).expect("serializable refs"),
            to_value(&after).expect("serializable refs"),
            to_value(config).expect("serializable config"),
        ])
    );
    let mut seen = HashSet::new();
    let candidate_message_ids = candidate
        .iter()
        .filter(|reference| seen.insert(reference.message_id.clone()))
        .map(|reference| reference.message_id.clone())
        .collect();
    let candidate_token_count = refs_tokens(candidate.iter());
    let total_token_count = refs_tokens(before.iter().chain(candidate.iter()).chain(after.iter()));
    ExtractionWindow {
        id,
        scope_id: episode.scope_id.clone(),
        session_id: episode.session_id.clone(),
        episode_id: episode.id.clone(),
        candidate_refs: candidate,
        context_before_refs: before,
        context_after_refs: after,
        candidate_message_ids,
        candidate_token_count,
        total_token_count,
        window_version: config.version.clone(),
    }
}

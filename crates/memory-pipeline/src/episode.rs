use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::canonical::stable_hash;
use crate::error::{PipelineError, Result};
use crate::models::{ConversationEpisode, NormalizedMessage};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EpisodeConfig {
    pub max_time_gap_minutes: Option<i64>,
    pub metadata_boundary_fields: Vec<String>,
    pub version: String,
}

impl Default for EpisodeConfig {
    fn default() -> Self {
        Self {
            max_time_gap_minutes: None,
            metadata_boundary_fields: Vec::new(),
            version: "episode_v1".into(),
        }
    }
}

pub fn build_episodes(
    messages: &[NormalizedMessage],
    config: &EpisodeConfig,
) -> Result<Vec<ConversationEpisode>> {
    if config
        .max_time_gap_minutes
        .is_some_and(|minutes| minutes < 0)
    {
        return Err(PipelineError::InvalidInput(
            "max_time_gap_minutes must be non-negative".into(),
        ));
    }
    if messages.is_empty() {
        return Ok(Vec::new());
    }
    let mut episodes = Vec::new();
    let mut current = Vec::new();
    let mut current_reason = "start".to_owned();
    for message in messages {
        let reason = current
            .last()
            .and_then(|previous| boundary_reason(previous, message, config));
        if let Some(reason) = reason {
            episodes.push(make_episode(&current, &current_reason, config));
            current.clear();
            current_reason = reason;
        }
        current.push(message.clone());
    }
    episodes.push(make_episode(&current, &current_reason, config));
    Ok(episodes)
}

fn boundary_reason(
    previous: &NormalizedMessage,
    current: &NormalizedMessage,
    config: &EpisodeConfig,
) -> Option<String> {
    if previous.scope_id != current.scope_id {
        return Some("scope_change".into());
    }
    if previous.session_id != current.session_id {
        return Some("session_change".into());
    }
    for field in &config.metadata_boundary_fields {
        let previous_value = previous
            .metadata
            .get(field)
            .filter(|value| !value.is_null());
        let current_value = current.metadata.get(field).filter(|value| !value.is_null());
        if previous_value != current_value {
            return Some(format!("metadata_change:{field}"));
        }
    }
    if let (Some(limit), Some(previous), Some(current)) = (
        config.max_time_gap_minutes,
        parse_time(&previous.timestamp),
        parse_time(&current.timestamp),
    ) {
        if current.signed_duration_since(previous).num_seconds() as f64 / 60.0 > limit as f64 {
            return Some("time_gap".into());
        }
    }
    None
}

fn make_episode(
    messages: &[NormalizedMessage],
    boundary_reason: &str,
    config: &EpisodeConfig,
) -> ConversationEpisode {
    let first = &messages[0];
    let refs = messages
        .iter()
        .map(|message| {
            json!({
                "message_id": message.id,
                "text_hash": stable_hash(&[json!(message.text)]),
            })
        })
        .collect::<Vec<_>>();
    let id = format!(
        "episode-{}",
        stable_hash(&[
            json!(first.scope_id),
            json!(first.session_id),
            json!(refs),
            json!({
                "max_time_gap_minutes": config.max_time_gap_minutes,
                "metadata_boundary_fields": config.metadata_boundary_fields,
                "version": config.version,
            }),
        ])
    );
    ConversationEpisode {
        id,
        scope_id: first.scope_id.clone(),
        session_id: first.session_id.clone(),
        message_ids: messages.iter().map(|message| message.id.clone()).collect(),
        start_time: first.timestamp.clone(),
        end_time: messages
            .last()
            .expect("episode is non-empty")
            .timestamp
            .clone(),
        boundary_reason: boundary_reason.into(),
        episode_version: config.version.clone(),
    }
}

pub(crate) fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    if value.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            DateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f%:z")
                .ok()
                .map(|value| value.with_timezone(&Utc))
        })
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|value| value.and_utc())
        })
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|value| value.and_utc())
        })
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()
                .and_then(|value| value.and_hms_opt(0, 0, 0))
                .map(|value| value.and_utc())
        })
}

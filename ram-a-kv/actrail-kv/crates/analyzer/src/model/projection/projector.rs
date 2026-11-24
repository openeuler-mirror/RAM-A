//! Projector applies protocol field policy without classifying UUIDs, timestamps, or business fields.

use serde_json::{Map, Value};

use crate::model::comparison::ComparisonGroupKey;
use crate::model::corpus::CorpusRecord;

use super::{
    CacheSequence, CacheUnit, CacheUnitKind, ContextCollectionKind, HierarchyLocation,
    ProjectionSkip, ProjectionSkipReason, SourceLocation,
};

const DIALECT: &str = "openai-compatible-chat";
const ADAPTER_REVISION: &str = "1";

#[derive(Clone, Debug)]
pub struct ProjectionLimits {
    pub max_units: usize,
    pub max_text_unit_bytes: usize,
}

impl Default for ProjectionLimits {
    fn default() -> Self {
        Self {
            max_units: 512,
            max_text_unit_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RequestProjector {
    limits: ProjectionLimits,
    comparison_window_seconds: u64,
}

impl RequestProjector {
    pub fn new(limits: ProjectionLimits, comparison_window_seconds: u64) -> Self {
        Self {
            limits,
            comparison_window_seconds,
        }
    }

    pub fn project(&self, record: &CorpusRecord) -> Result<CacheSequence, ProjectionSkip> {
        self.project_inner(record).map_err(|reason| ProjectionSkip {
            request_id: record.id.clone(),
            reason,
        })
    }

    fn project_inner(&self, record: &CorpusRecord) -> Result<CacheSequence, ProjectionSkipReason> {
        let payload = record
            .payload
            .as_object()
            .ok_or(ProjectionSkipReason::PayloadNotObject)?;
        let _model = payload
            .get("model")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or(ProjectionSkipReason::MissingModel)?;
        let messages = payload
            .get("messages")
            .ok_or(ProjectionSkipReason::MissingMessages)?
            .as_array()
            .ok_or(ProjectionSkipReason::MessagesNotArray)?;
        let domain = ComparisonGroupKey::from_record(
            record,
            self.comparison_window_seconds,
            DIALECT,
            ADAPTER_REVISION,
            "openai-compatible-chat/v1",
        )
        .map_err(|error| ProjectionSkipReason::InvalidComparisonGroup(format!("{error:?}")))?;
        let mut units = Vec::new();

        if let Some(tools) = payload.get("tools").and_then(Value::as_array) {
            push_structural(&mut units, "tools:start", "$.tools");
            for (index, tool) in tools.iter().enumerate() {
                let path = format!("$.tools[{index}]");
                let identity = tool_identity(tool);
                push_unit(
                    &mut units,
                    CacheUnitKind::ToolDefinition,
                    "tool-definition",
                    canonical_json(tool),
                    &path,
                    None,
                    None,
                    identity,
                );
                self.ensure_unit_budget(&units)?;
            }
            push_structural(&mut units, "tools:end", "$.tools");
        }

        push_structural(&mut units, "messages:start", "$.messages");
        for (index, message) in messages.iter().enumerate() {
            let object = message
                .as_object()
                .ok_or(ProjectionSkipReason::MessageNotObject { index })?;
            let role = object
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let base = format!("$.messages[{index}]");
            push_unit(
                &mut units,
                CacheUnitKind::Role,
                "message:role",
                role.to_owned(),
                &format!("{base}.role"),
                Some(index),
                Some(role),
                None,
            );
            self.ensure_unit_budget(&units)?;
            // This order is adapter policy, independent of HTTP object serialization order.
            for field in [
                "name",
                "content",
                "function_call",
                "tool_calls",
                "tool_call_id",
                "refusal",
            ] {
                if let Some(value) = object.get(field) {
                    if field == "content" {
                        if let Some(blocks) = value.as_array() {
                            self.project_content_blocks(&mut units, index, role, blocks)?;
                            continue;
                        }
                    }
                    self.project_message_field(&mut units, index, role, field, value)?;
                }
            }
        }
        push_structural(&mut units, "messages:end", "$.messages");
        self.ensure_unit_budget(&units)?;
        Ok(CacheSequence {
            request_id: record.id.clone(),
            domain,
            units,
            projection_reliability_millis: 1000,
        })
    }

    fn project_content_blocks(
        &self,
        units: &mut Vec<CacheUnit>,
        message_index: usize,
        role: &str,
        blocks: &[Value],
    ) -> Result<(), ProjectionSkipReason> {
        for (block_index, block) in blocks.iter().enumerate() {
            let base = format!("$.messages[{message_index}].content[{block_index}]");
            let (content, path, exact_string) = content_block_value(block, &base);
            if content.len() > self.limits.max_text_unit_bytes {
                return Err(ProjectionSkipReason::TextBudgetExceeded {
                    path,
                    bytes: content.len(),
                    limit: self.limits.max_text_unit_bytes,
                });
            }
            let block_type = block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            push_unit(
                units,
                CacheUnitKind::VisibleText,
                &format!("message:{role}:content-block:{block_type}"),
                content,
                &path,
                Some(message_index),
                Some(role),
                None,
            );
            let unit = units
                .last_mut()
                .expect("a projected content block was just appended");
            unit.hierarchy = HierarchyLocation {
                collection: ContextCollectionKind::ContentBlocks,
                parent_json_path: format!("$.messages[{message_index}].content"),
                element_index: Some(message_index),
                content_block_index: Some(block_index),
            };
            if !exact_string {
                unit.source.utf8_bytes = None;
            }
            self.ensure_unit_budget(units)?;
        }
        Ok(())
    }

    fn project_message_field(
        &self,
        units: &mut Vec<CacheUnit>,
        message_index: usize,
        role: &str,
        field: &str,
        value: &Value,
    ) -> Result<(), ProjectionSkipReason> {
        let path = format!("$.messages[{message_index}].{field}");
        let content = value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| canonical_json(value));
        if content.len() > self.limits.max_text_unit_bytes {
            return Err(ProjectionSkipReason::TextBudgetExceeded {
                path,
                bytes: content.len(),
                limit: self.limits.max_text_unit_bytes,
            });
        }
        let kind = match field {
            "tool_calls" | "function_call" => CacheUnitKind::ToolCall,
            "tool_call_id" if role == "tool" => CacheUnitKind::ToolResult,
            _ => CacheUnitKind::VisibleText,
        };
        let identity = if matches!(kind, CacheUnitKind::ToolCall | CacheUnitKind::ToolResult) {
            tool_identity(value)
        } else {
            None
        };
        push_unit(
            units,
            kind,
            &format!("message:{role}:{field}"),
            content,
            &path,
            Some(message_index),
            Some(role),
            identity,
        );
        if !value.is_string() {
            // Canonical JSON is an observable structural proxy, not a byte slice of a source string.
            units
                .last_mut()
                .expect("a projected message field was just appended")
                .source
                .utf8_bytes = None;
        }
        self.ensure_unit_budget(units)?;
        Ok(())
    }

    fn ensure_unit_budget(&self, units: &[CacheUnit]) -> Result<(), ProjectionSkipReason> {
        if units.len() > self.limits.max_units {
            Err(ProjectionSkipReason::UnitBudgetExceeded {
                limit: self.limits.max_units,
            })
        } else {
            Ok(())
        }
    }
}

fn content_block_value(block: &Value, base: &str) -> (String, String, bool) {
    if let Some(text) = block.as_str() {
        return (text.to_owned(), base.to_owned(), true);
    }
    if let Some(object) = block.as_object() {
        for field in ["text", "input_text", "output_text", "content"] {
            if let Some(text) = object.get(field).and_then(Value::as_str) {
                return (text.to_owned(), format!("{base}.{field}"), true);
            }
        }
    }
    (canonical_json(block), base.to_owned(), false)
}

impl Default for RequestProjector {
    fn default() -> Self {
        Self::new(ProjectionLimits::default(), 3_600)
    }
}

#[allow(clippy::too_many_arguments)]
fn push_unit(
    units: &mut Vec<CacheUnit>,
    kind: CacheUnitKind,
    alignment_key: &str,
    content: String,
    path: &str,
    message_index: Option<usize>,
    role: Option<&str>,
    tool_identity: Option<String>,
) {
    let byte_range = matches!(kind, CacheUnitKind::VisibleText).then_some(0..content.len());
    units.push(CacheUnit {
        kind,
        alignment_key: alignment_key.to_owned(),
        content,
        source: SourceLocation {
            json_path: path.to_owned(),
            utf8_bytes: byte_range,
            message_index,
            role: role.map(str::to_owned),
        },
        tool_identity,
        hierarchy: hierarchy_for(path, message_index),
    });
}

fn hierarchy_for(path: &str, message_index: Option<usize>) -> HierarchyLocation {
    if path.starts_with("$.tools") {
        HierarchyLocation {
            collection: ContextCollectionKind::Tools,
            parent_json_path: "$.tools".to_owned(),
            element_index: path
                .strip_prefix("$.tools[")
                .and_then(|rest| rest.split(']').next())
                .and_then(|index| index.parse().ok()),
            content_block_index: None,
        }
    } else if path.starts_with("$.messages") {
        HierarchyLocation {
            collection: ContextCollectionKind::Messages,
            parent_json_path: "$.messages".to_owned(),
            element_index: message_index,
            content_block_index: None,
        }
    } else {
        HierarchyLocation {
            collection: ContextCollectionKind::Request,
            parent_json_path: "$".to_owned(),
            element_index: None,
            content_block_index: None,
        }
    }
}

fn push_structural(units: &mut Vec<CacheUnit>, marker: &str, path: &str) {
    push_unit(
        units,
        CacheUnitKind::Structural,
        marker,
        marker.to_owned(),
        path,
        None,
        None,
        None,
    );
}

fn tool_identity(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    object
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| {
            object
                .get("function")
                .and_then(Value::as_object)
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(object) => canonical_object(object),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => serde_json::to_string(value).expect("parsed JSON is serializable"),
    }
}

fn canonical_object(object: &Map<String, Value>) -> String {
    let mut keys: Vec<_> = object.keys().collect();
    keys.sort_unstable();
    let entries = keys.into_iter().map(|key| {
        format!(
            "{}:{}",
            serde_json::to_string(key).expect("JSON key is serializable"),
            canonical_json(&object[key])
        )
    });
    format!("{{{}}}", entries.collect::<Vec<_>>().join(","))
}

#[cfg(test)]
#[path = "../../../../../tests/unit/analyzer/model/projection/projector.rs"]
mod tests;

//! NDJSON corpus loader enforces byte/count budgets and reports skips instead of hiding loss.

use std::{
    collections::BTreeMap,
    io::{self, BufRead},
};

use actrail_kv_artifacts::CapturedRequest;
use futures_util::stream::{Stream, StreamExt};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    selection::SelectionCollector, CaptureComparison, CorpusLoadResult, CorpusRecord, CorpusSkip,
    CorpusSkipReason, ReportSelection, ResponseUsage,
};

pub use analysis_contract::config::CorpusConfig;

#[derive(Clone, Debug, Default)]
pub struct CorpusLoader {
    limits: CorpusConfig,
}

/// Reading the corpus failed: the caller must treat the load as failed instead
/// of retrying the same unreadable offset.
#[derive(Debug, thiserror::Error)]
#[error("failed to read captured requests at line {input_line}")]
pub struct CorpusReadError {
    pub input_line: usize,
    #[source]
    pub source: io::Error,
}

impl CorpusLoader {
    pub fn new(limits: CorpusConfig) -> Self {
        Self { limits }
    }

    pub fn load<R: BufRead>(&self, reader: R) -> Result<CorpusLoadResult, CorpusReadError> {
        let mut loaded = CorpusAccumulator::new(&self.limits);
        for (zero_based, line_result) in reader.split(b'\n').enumerate() {
            let input_line = zero_based + 1;
            let line = match line_result {
                Ok(line) => line,
                Err(error) => {
                    // A read failure does not advance the underlying cursor, so
                    // every retry observes the same error again: skipping the
                    // line loops forever at the same offset (observed on Windows
                    // as an unbounded memory climb while the receiver held the
                    // output). Abort with the I/O error so callers fail fast.
                    return Err(CorpusReadError {
                        input_line,
                        source: error,
                    });
                }
            };
            loaded.push(&line, input_line);
        }
        Ok(loaded.result)
    }

    pub(crate) async fn load_captures<S, E>(&self, records: S) -> anyhow::Result<CorpusLoadResult>
    where
        S: Stream<Item = Result<CapturedRequest, E>>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let mut loaded = CorpusAccumulator::new(&self.limits);
        let mut records = std::pin::pin!(records.enumerate());
        while let Some((position, record)) = records.next().await {
            if loaded.result.corpus.records.len() >= self.limits.max_records {
                anyhow::bail!(
                    "capture scan exceeds max_records ({})",
                    self.limits.max_records
                );
            }
            loaded.push(&serde_json::to_vec(&record?)?, position + 1);
        }
        Ok(loaded.result)
    }

    pub(crate) fn load_selected<R: BufRead>(
        &self,
        reader: R,
        selection: &ReportSelection,
    ) -> anyhow::Result<CorpusLoadResult> {
        let mut loaded = CorpusAccumulator::new(&self.limits);
        let mut selected = SelectionCollector::new(self.limits.max_records);
        for (position, line) in reader.split(b'\n').enumerate() {
            let input_line = position + 1;
            let line = line.map_err(|source| CorpusReadError { input_line, source })?;
            let Ok(document) = decode_record(&line) else {
                continue;
            };
            let Some(matched) = selection.matches(
                document.get("session_key").and_then(Value::as_str),
                document.get("captured_at").and_then(Value::as_str),
            ) else {
                continue;
            };
            if let Some((bytes, document)) =
                selected.accept(matched, input_line, (line.len(), document))?
            {
                loaded.push_with(bytes, input_line, || Ok(document));
            }
        }
        for (input_line, (bytes, document)) in selected.into_latest() {
            loaded.push_with(bytes, input_line, || Ok(document));
        }
        Ok(loaded.result)
    }

    pub(crate) async fn load_selected_captures<S, E>(
        &self,
        records: S,
        selection: &ReportSelection,
    ) -> anyhow::Result<CorpusLoadResult>
    where
        S: Stream<Item = Result<CapturedRequest, E>>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let mut loaded = CorpusAccumulator::new(&self.limits);
        let mut selected = SelectionCollector::new(self.limits.max_records);
        let mut records = std::pin::pin!(records.enumerate());
        while let Some((position, record)) = records.next().await {
            let record = record?;
            let input_line = position + 1;
            let Some(matched) =
                selection.matches(record.session_key.as_deref(), Some(&record.captured_at))
            else {
                continue;
            };
            if let Some(record) = selected.accept(matched, input_line, record)? {
                loaded.push(&serde_json::to_vec(&record)?, input_line);
            }
        }
        for (input_line, record) in selected.into_latest() {
            loaded.push(&serde_json::to_vec(&record)?, input_line);
        }
        Ok(loaded.result)
    }
}

struct CorpusAccumulator<'a> {
    limits: &'a CorpusConfig,
    result: CorpusLoadResult,
    occurrences: BTreeMap<String, usize>,
}

impl<'a> CorpusAccumulator<'a> {
    fn new(limits: &'a CorpusConfig) -> Self {
        Self {
            limits,
            result: CorpusLoadResult::default(),
            occurrences: BTreeMap::new(),
        }
    }

    fn push(&mut self, line: &[u8], input_line: usize) {
        if line.iter().all(u8::is_ascii_whitespace) {
            self.result.skipped.push(CorpusSkip {
                input_line,
                reason: CorpusSkipReason::EmptyLine,
            });
            return;
        }
        self.push_with(line.len(), input_line, || decode_record(line));
    }

    fn push_with(
        &mut self,
        bytes: usize,
        input_line: usize,
        decode: impl FnOnce() -> Result<Value, CorpusSkipReason>,
    ) {
        let reason = if bytes > self.limits.max_record_bytes {
            CorpusSkipReason::RecordTooLarge {
                bytes,
                limit: self.limits.max_record_bytes,
            }
        } else if self.result.corpus.records.len() >= self.limits.max_records {
            CorpusSkipReason::RecordBudgetExceeded {
                limit: self.limits.max_records,
            }
        } else {
            match decode().and_then(|document| parse_record(document, input_line)) {
                Ok(mut record) => {
                    let occurrence = self.occurrences.entry(record.id.clone()).or_default();
                    record.id = format!("{}:{occurrence}", record.id);
                    *occurrence += 1;
                    self.result.corpus.records.push(record);
                    return;
                }
                Err(reason) => reason,
            }
        };
        self.result.skipped.push(CorpusSkip { input_line, reason });
    }
}

fn decode_record(line: &[u8]) -> Result<Value, CorpusSkipReason> {
    serde_json::from_slice(line).map_err(|error| CorpusSkipReason::InvalidJson(error.to_string()))
}

fn parse_record(record: Value, input_line: usize) -> Result<CorpusRecord, CorpusSkipReason> {
    let object = record
        .as_object()
        .ok_or(CorpusSkipReason::RecordNotObject)?;
    let payload = object
        .get("payload")
        .ok_or(CorpusSkipReason::MissingPayload)?;
    if !payload.is_object() {
        return Err(CorpusSkipReason::PayloadNotObject);
    }
    let comparison = object
        .get("comparison")
        .and_then(Value::as_object)
        .ok_or(CorpusSkipReason::MissingComparison)?;
    let endpoint_key = comparison
        .get("endpoint_key")
        .and_then(Value::as_str)
        .ok_or(CorpusSkipReason::MissingEndpointKey)?;
    let endpoint_key = endpoint_key.trim();
    if endpoint_key.is_empty() {
        return Err(CorpusSkipReason::EmptyEndpointKey);
    }
    let agent_key = optional_comparison_key(comparison, "agent_key")?;
    let model_deployment_key = optional_comparison_key(comparison, "model_deployment_key")?;
    let kv_namespace = optional_comparison_key(comparison, "kv_namespace")?;
    let session_key = match object.get("session_key") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.trim().to_owned()),
        Some(Value::String(_)) => return Err(CorpusSkipReason::EmptySessionKey),
        Some(_) => return Err(CorpusSkipReason::InvalidSessionKey),
    };
    let response_usage = payload
        .get("_actrail_response_usage")
        .and_then(Value::as_object)
        .and_then(parse_response_usage);
    // Capture metadata is part of request identity and comparison grouping, not model context.
    let canonical = canonical_json(&record);
    let mut digest = Sha256::new();
    digest.update(canonical.as_bytes());
    let id = hex::encode(digest.finalize());
    Ok(CorpusRecord {
        id,
        captured_at: object
            .get("captured_at")
            .and_then(Value::as_str)
            .map(str::to_owned),
        source: object
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_owned),
        session_key,
        comparison: CaptureComparison {
            endpoint_key: endpoint_key.to_owned(),
            agent_key,
            model_deployment_key,
            kv_namespace,
        },
        payload: payload.clone(),
        response_usage,
        input_line,
    })
}

/// 解析 payload 保留字段 `_actrail_response_usage`。
///
/// 约定见 `docs/reference/receiver/input.md`。
/// - `prompt_tokens` 与 `input_tokens` 是同一字段的两种写法，都表示**总输入**（含命中部分）；
/// - `completion_tokens` 与 `output_tokens` 同理；
/// - `ttft_ms` / `total_ms` 缺失时为 `None`，不按 0 处理；
/// - **缺少总输入数时返回 `None`**：没有分母就算不出命中率，按"没有 usage"处理，
///   让该请求走估算口径，避免产出看起来真实的 0%。
fn parse_response_usage(usage: &serde_json::Map<String, Value>) -> Option<ResponseUsage> {
    let total_input = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_u64)?;
    let completion_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached_tokens = usage
        .get("cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some(ResponseUsage {
        prompt_tokens: token_count(total_input),
        cached_tokens: token_count(cached_tokens),
        completion_tokens: token_count(completion_tokens),
        ttft_ms: usage.get("ttft_ms").and_then(Value::as_u64),
        total_ms: usage.get("total_ms").and_then(Value::as_u64),
    })
}

/// token 计数按 u32 收敛；超过上限的记录按上限处理，不做回绕。
fn token_count(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn optional_comparison_key(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>, CorpusSkipReason> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_str()
        .ok_or_else(|| CorpusSkipReason::InvalidComparisonKey {
            field: key.to_owned(),
        })?
        .trim();
    if value.is_empty() {
        return Err(CorpusSkipReason::EmptyComparisonKey {
            field: key.to_owned(),
        });
    }
    Ok(Some(value.to_owned()))
}

pub(crate) fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort_unstable();
            let entries = keys.into_iter().map(|key| {
                format!(
                    "{}:{}",
                    serde_json::to_string(key).expect("JSON object key is serializable"),
                    canonical_json(&object[key])
                )
            });
            format!("{{{}}}", entries.collect::<Vec<_>>().join(","))
        }
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => serde_json::to_string(value).expect("parsed JSON value is serializable"),
    }
}

#[cfg(test)]
#[path = "../../../../../tests/unit/analyzer/model/corpus/loader.rs"]
mod tests;

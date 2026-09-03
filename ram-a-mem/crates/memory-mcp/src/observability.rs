use std::collections::BTreeMap;
use std::env;
use std::fmt;

use chrono::{SecondsFormat, Utc};
use serde_json::{Map, Value};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, JsonFields, Writer};
use tracing_subscriber::fmt::{FmtContext, FormattedFields};
use tracing_subscriber::registry::LookupSpan;

pub const LOG_FORMAT_ENV: &str = "RAM_A_LOG_FORMAT";
pub const LOG_SOURCE_ENV: &str = "RAM_A_LOG_SOURCE";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogFormat {
    Json,
    Compact,
}

impl LogFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Compact => "compact",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogSettings {
    pub format: LogFormat,
    pub source: bool,
}

impl Default for LogSettings {
    fn default() -> Self {
        Self {
            format: LogFormat::Json,
            source: false,
        }
    }
}

impl LogSettings {
    pub fn from_env() -> Result<Self, LogSettingsError> {
        Ok(Self {
            format: parse_format(env::var_os(LOG_FORMAT_ENV))?,
            source: parse_source(env::var_os(LOG_SOURCE_ENV))?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogSettingsError {
    InvalidFormat,
    InvalidSource,
}

impl fmt::Display for LogSettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidFormat => "invalid RAM_A_LOG_FORMAT; expected one of: json, compact",
            Self::InvalidSource => "invalid RAM_A_LOG_SOURCE; expected one of: true, false",
        })
    }
}

impl std::error::Error for LogSettingsError {}

fn parse_format(value: Option<std::ffi::OsString>) -> Result<LogFormat, LogSettingsError> {
    match value {
        None => Ok(LogFormat::Json),
        Some(value) if value == "json" => Ok(LogFormat::Json),
        Some(value) if value == "compact" => Ok(LogFormat::Compact),
        Some(_) => Err(LogSettingsError::InvalidFormat),
    }
}

fn parse_source(value: Option<std::ffi::OsString>) -> Result<bool, LogSettingsError> {
    match value {
        None => Ok(false),
        Some(value) if value == "true" => Ok(true),
        Some(value) if value == "false" => Ok(false),
        Some(_) => Err(LogSettingsError::InvalidSource),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RamLogFormatter {
    settings: LogSettings,
}

impl RamLogFormatter {
    pub const fn new(settings: LogSettings) -> Self {
        Self { settings }
    }
}

impl<S> FormatEvent<S, JsonFields> for RamLogFormatter
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, JsonFields>,
        writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut fields = collect_fields(ctx, event)?;
        enrich_ram_event(event, &mut fields);
        match self.settings.format {
            LogFormat::Json => self.format_json(writer, event, fields),
            LogFormat::Compact => self.format_compact(writer, event, fields),
        }
    }
}

impl RamLogFormatter {
    fn format_json(
        &self,
        mut writer: Writer<'_>,
        event: &Event<'_>,
        fields: BTreeMap<String, Value>,
    ) -> fmt::Result {
        let metadata = event.metadata();
        let mut record = Map::new();
        record.insert("timestamp".into(), Value::String(timestamp()));
        record.insert("level".into(), Value::String(metadata.level().to_string()));
        record.insert(
            "target".into(),
            Value::String(metadata.target().to_string()),
        );
        record.insert(
            "fields".into(),
            Value::Object(fields.into_iter().collect::<Map<_, _>>()),
        );
        if self.settings.source {
            if let Some(file) = metadata.file() {
                record.insert("filename".into(), Value::String(file.to_string()));
            }
            if let Some(line) = metadata.line() {
                record.insert("line_number".into(), Value::Number(line.into()));
            }
        }
        let encoded = serde_json::to_string(&Value::Object(record)).map_err(|_| fmt::Error)?;
        writeln!(writer, "{encoded}")
    }

    fn format_compact(
        &self,
        mut writer: Writer<'_>,
        event: &Event<'_>,
        mut fields: BTreeMap<String, Value>,
    ) -> fmt::Result {
        let metadata = event.metadata();
        let failure = is_failure(metadata.level(), &fields);
        let operation = take_string(&mut fields, "operation").unwrap_or_else(|| "event".into());
        let stage = take_string(&mut fields, "stage");
        let context = stage
            .map(|stage| format!("{operation}/{stage}"))
            .unwrap_or(operation);
        let message = take_string(&mut fields, "message").unwrap_or_else(|| "event emitted".into());

        write!(writer, "[{}] [{}] ", timestamp(), metadata.level())?;
        if failure {
            let origin_file = take_string(&mut fields, "error_origin_file")
                .or_else(|| metadata.file().map(str::to_owned))
                .unwrap_or_else(|| "unknown".into());
            let origin_line = take_u64(&mut fields, "error_origin_line")
                .or_else(|| metadata.line().map(u64::from))
                .unwrap_or_default();
            write!(writer, "[{origin_file}:{origin_line}] [{context}] ")?;
            let code = take_string(&mut fields, "error_code").unwrap_or_else(|| "ERROR".into());
            let summary = take_string(&mut fields, "source_error_message").unwrap_or(message);
            write!(writer, "{code}: {}", single_line(&summary))?;
            fields.insert(
                "target".into(),
                Value::String(metadata.target().to_string()),
            );
        } else {
            write!(
                writer,
                "[{}] [{}] {}",
                metadata.target(),
                context,
                single_line(&message)
            )?;
        }

        if self.settings.source {
            if let (Some(file), Some(line)) = (metadata.file(), metadata.line()) {
                fields.insert("log_at".into(), Value::String(format!("{file}:{line}")));
            }
        }
        write_diagnostics(&mut writer, fields)
    }
}

fn collect_fields<S>(
    ctx: &FmtContext<'_, S, JsonFields>,
    event: &Event<'_>,
) -> Result<BTreeMap<String, Value>, fmt::Error>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    let mut fields = BTreeMap::new();
    if let Some(scope) = ctx.event_scope() {
        for span in scope.from_root() {
            let extensions = span.extensions();
            if let Some(formatted) = extensions.get::<FormattedFields<JsonFields>>() {
                merge_json_fields(&mut fields, &formatted.fields)?;
            }
        }
    }

    let mut encoded = String::new();
    ctx.format_fields(Writer::new(&mut encoded), event)?;
    merge_json_fields(&mut fields, &encoded)?;
    Ok(fields)
}

fn merge_json_fields(
    target: &mut BTreeMap<String, Value>,
    encoded: &str,
) -> Result<(), fmt::Error> {
    if encoded.is_empty() {
        return Ok(());
    }
    let fields =
        serde_json::from_str::<BTreeMap<String, Value>>(encoded).map_err(|_| fmt::Error)?;
    target.extend(fields);
    Ok(())
}

fn enrich_ram_event(event: &Event<'_>, fields: &mut BTreeMap<String, Value>) {
    let Some(event_name) = fields
        .get("event")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    if !event_name.starts_with("ram_a.") {
        return;
    }
    fields
        .entry("message".into())
        .or_insert_with(|| Value::String(default_message(&event_name)));
    if !fields.contains_key("operation") {
        let operation = default_operation(&event_name, fields);
        fields.insert("operation".into(), Value::String(operation));
    }

    if is_failure(event.metadata().level(), fields) {
        if !fields.contains_key("error_site") {
            let error_site = default_error_site(&event_name, fields);
            fields.insert("error_site".into(), Value::String(error_site));
        }
        if let Some(file) = event.metadata().file() {
            fields
                .entry("error_origin_file".into())
                .or_insert_with(|| Value::String(file.to_string()));
        }
        if let Some(line) = event.metadata().line() {
            fields
                .entry("error_origin_line".into())
                .or_insert_with(|| Value::Number(line.into()));
        }
        if !fields.contains_key("source_error_kind") {
            let error_kind = default_error_kind(fields).to_string();
            fields.insert("source_error_kind".into(), Value::String(error_kind));
        }
        if !fields.contains_key("source_error_message") {
            let summary = default_error_summary(fields);
            fields.insert("source_error_message".into(), Value::String(summary));
        }
    }
}

fn default_message(event: &str) -> String {
    if event.ends_with(".started") {
        "operation started".into()
    } else if event.ends_with(".completed") {
        "operation completed".into()
    } else if event.ends_with(".failed") {
        "operation failed".into()
    } else if event.ends_with(".retry") {
        "retrying provider request".into()
    } else if event.ends_with(".degraded") {
        "operation completed in degraded mode".into()
    } else {
        event
            .rsplit('.')
            .next()
            .unwrap_or("event")
            .replace('_', " ")
    }
}

fn default_operation(event: &str, fields: &BTreeMap<String, Value>) -> String {
    if let Some(tool) = fields.get("tool").and_then(Value::as_str) {
        return tool.to_string();
    }
    if let Some(operation) = fields.get("operation").and_then(Value::as_str) {
        return operation.to_string();
    }
    if event.starts_with("ram_a.memory.ingest") {
        "memory_ingest".into()
    } else if event.starts_with("ram_a.memory.search") {
        "memory_search".into()
    } else if event.starts_with("ram_a.provider") {
        "provider".into()
    } else if event.starts_with("ram_a.case") {
        "memory_case".into()
    } else if event.starts_with("ram_a.startup") {
        "startup".into()
    } else {
        "ram_a".into()
    }
}

fn default_error_site(event: &str, fields: &BTreeMap<String, Value>) -> String {
    let operation = default_operation(event, fields).replace('_', ".");
    let stage = fields
        .get("stage")
        .and_then(Value::as_str)
        .unwrap_or("failed");
    format!("ram_a.{operation}.{stage}")
}

fn default_error_kind(fields: &BTreeMap<String, Value>) -> &'static str {
    match fields.get("error_code").and_then(Value::as_str) {
        Some("SQLITE_BUSY") => "sqlite_busy",
        Some("SQLITE_READONLY") => "sqlite_readonly",
        Some("EMBEDDING_FAILED") => "embedding",
        Some("RERANK_FAILED") => "rerank",
        Some("CANCELLED") => "cancelled",
        _ => "internal",
    }
}

fn default_error_summary(fields: &BTreeMap<String, Value>) -> String {
    match fields.get("error_code").and_then(Value::as_str) {
        Some("INVALID_REQUEST") => "request validation failed",
        Some("IDEMPOTENCY_CONFLICT") => "idempotency key conflicts with an earlier request",
        Some("PIPELINE_FAILED") => "memory pipeline failed",
        Some("PIPELINE_EXTRACT_FAILED") => "memory extraction failed",
        Some("PIPELINE_GROUND_FAILED") => "memory grounding failed",
        Some("RERANK_FAILED") => "memory rerank failed",
        Some("EMBEDDING_FAILED") => "embedding provider failed",
        Some("IDEMPOTENCY_STORAGE_FAILED") => "idempotency storage failed",
        Some("SQLITE_BUSY") => "SQLite database is busy",
        Some("SQLITE_READONLY") => "SQLite database is read-only",
        Some("VECTOR_PERSIST_FAILED") => "memory vector persistence failed",
        Some("STORAGE_FAILED") => "memory storage failed",
        Some("CANCELLED") => "operation was cancelled",
        Some(_) | None => "operation failed",
    }
    .into()
}

fn is_failure(level: &tracing::Level, fields: &BTreeMap<String, Value>) -> bool {
    *level == tracing::Level::ERROR
        || fields.contains_key("error_code")
        || fields
            .get("event")
            .and_then(Value::as_str)
            .is_some_and(|event| event.ends_with(".failed"))
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn take_string(fields: &mut BTreeMap<String, Value>, key: &str) -> Option<String> {
    fields.remove(key).and_then(|value| match value {
        Value::String(value) => Some(value),
        Value::Null => None,
        value => Some(value.to_string()),
    })
}

fn take_u64(fields: &mut BTreeMap<String, Value>, key: &str) -> Option<u64> {
    fields.remove(key).and_then(|value| value.as_u64())
}

fn write_diagnostics(writer: &mut Writer<'_>, mut fields: BTreeMap<String, Value>) -> fmt::Result {
    for key in [
        "message",
        "operation",
        "stage",
        "error_code",
        "source_error_message",
        "error_origin_file",
        "error_origin_line",
    ] {
        fields.remove(key);
    }
    if fields.is_empty() {
        return writeln!(writer);
    }

    write!(writer, " |")?;
    for key in [
        "target",
        "error_site",
        "request_id",
        "pipeline_run_id",
        "retriable",
        "source_error_kind",
        "completed_units",
        "total_units",
        "elapsed_ms",
        "latency_ms",
        "log_at",
        "event",
    ] {
        if let Some(value) = fields.remove(key) {
            write!(writer, " {key}={}", display_value(&value))?;
        }
    }
    for (key, value) in fields {
        write!(writer, " {key}={}", display_value(&value))?;
    }
    writeln!(writer)
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(value) if needs_quotes(value) => {
            serde_json::to_string(value).unwrap_or_else(|_| "\"<invalid>\"".into())
        }
        Value::String(value) => single_line(value),
        value => single_line(&value.to_string()),
    }
}

fn needs_quotes(value: &str) -> bool {
    value.chars().any(|character| {
        character.is_whitespace() || matches!(character, '=' | '"' | '\\' | '|' | '[' | ']')
    })
}

fn single_line(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::prelude::*;

    use super::*;

    #[derive(Clone, Default)]
    struct LogBuffer(Arc<Mutex<Vec<u8>>>);

    struct LogWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("log buffer lock").extend(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for LogBuffer {
        type Writer = LogWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            LogWriter(self.0.clone())
        }
    }

    impl LogBuffer {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().expect("log buffer lock").clone())
                .expect("UTF-8 log output")
        }
    }

    fn capture(settings: LogSettings, emit: impl FnOnce()) -> String {
        let logs = LogBuffer::default();
        let layer = tracing_subscriber::fmt::layer()
            .fmt_fields(JsonFields::new())
            .event_format(RamLogFormatter::new(settings))
            .with_writer(logs.clone());
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, emit);
        logs.text()
    }

    fn emit_failure() {
        let span = tracing::info_span!(
            "ram_a.tool",
            request_id = "request-123",
            tool = "memory_ingest"
        );
        let _guard = span.enter();
        tracing::error!(
            event = "ram_a.memory.ingest.failed",
            stage = "vector_persist",
            error_code = "EMBEDDING_FAILED",
            retriable = true,
            error_site = "memory_mcp.ingest.vector_persist",
            error_origin_file = "crates/memory-core/src/embedding.rs",
            error_origin_line = 153_u64,
            source_error_kind = "timeout",
            source_error_message = "embedding provider timed out"
        );
    }

    #[test]
    fn settings_are_strict_and_have_production_defaults() {
        assert_eq!(parse_format(None).unwrap(), LogFormat::Json);
        assert!(!parse_source(None).unwrap());
        assert_eq!(
            parse_format(Some("compact".into())).unwrap(),
            LogFormat::Compact
        );
        assert!(parse_source(Some("true".into())).unwrap());

        for invalid in ["", "JSON", " compact", "text"] {
            assert_eq!(
                parse_format(Some(invalid.into())),
                Err(LogSettingsError::InvalidFormat)
            );
        }
        for invalid in ["", "TRUE", "1", " false"] {
            assert_eq!(
                parse_source(Some(invalid.into())),
                Err(LogSettingsError::InvalidSource)
            );
        }
    }

    #[test]
    fn compact_values_are_unambiguous_and_single_line() {
        assert_eq!(display_value(&Value::String("plain".into())), "plain");
        assert_eq!(
            display_value(&Value::String("two words".into())),
            "\"two words\""
        );
        assert_eq!(display_value(&Value::String("a\nb".into())), "\"a\\nb\"");
        assert_eq!(single_line("a\rb\tc"), "a\\rb\\tc");
    }

    #[test]
    fn json_keeps_error_origin_when_source_is_disabled() {
        let output = capture(LogSettings::default(), emit_failure);
        let record: Value = serde_json::from_str(output.trim()).expect("JSON log record");

        assert_eq!(record["fields"]["request_id"], "request-123");
        assert_eq!(
            record["fields"]["error_origin_file"],
            "crates/memory-core/src/embedding.rs"
        );
        assert_eq!(record["fields"]["error_origin_line"], 153);
        assert!(record.get("filename").is_none());
        assert!(record.get("line_number").is_none());
    }

    #[test]
    fn compact_puts_origin_and_error_summary_before_diagnostics() {
        let output = capture(
            LogSettings {
                format: LogFormat::Compact,
                source: false,
            },
            emit_failure,
        );

        assert!(output.contains(
            "[ERROR] [crates/memory-core/src/embedding.rs:153] [memory_ingest/vector_persist] EMBEDDING_FAILED: embedding provider timed out |"
        ));
        assert!(output.contains("request_id=request-123"));
        assert!(output.contains("site=memory_mcp.ingest.vector_persist"));
        assert!(!output.contains("log_at="));
        assert_eq!(output.lines().count(), 1);
    }

    #[test]
    fn source_switch_adds_log_call_site_without_replacing_error_origin() {
        let output = capture(
            LogSettings {
                format: LogFormat::Compact,
                source: true,
            },
            emit_failure,
        );

        let normalized = output.replace('\\', "/");
        assert!(normalized.contains("[crates/memory-core/src/embedding.rs:153]"));
        assert!(normalized.contains("log_at="));
        assert!(normalized.contains("observability.rs:"));
    }
}

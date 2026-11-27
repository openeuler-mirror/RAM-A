// Copyright (c) 2026 Huawei Technologies Co.,Ltd. All rights reserved.
//
// RAM-A is licensed under Mulan PSL v2.
// You can use this software according to the terms and conditions of the Mulan
// PSL v2.
// You may obtain a copy of Mulan PSL v2 at:
//         http://license.coscl.org.cn/MulanPSL2
// THIS SOFTWARE IS PROVIDED ON AN "AS IS" BASIS, WITHOUT WARRANTIES OF ANY
// KIND, EITHER EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO
// NON-INFRINGEMENT, MERCHANTABILITY OR FIT FOR A PARTICULAR PURPOSE.
// See the Mulan PSL v2 for more details.

//! Actrail-kv analysis and report endpoints: POST /actrail/analyze and
//! POST /actrail/report. Each analyze request creates a short-lived SDK
//! instance (loads .so, runs the operation, closes). Operations are
//! synchronous inside the SDK so they run on spawn_blocking to avoid
//! stalling the async runtime. Report uses the actrail-kv-reporter crate
//! directly (it shares the workspace) to read analysis.json and write the
//! rendered report file.

use actrail_kv_sdk::{
    AnalyzerFactory, RecordLimit, SdkConfig, SdkOverrides, Selection, TimeRange,
};
use analysis_contract::settings::AnalyzerSettingsOverrides;
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::api::EventResponse;
use crate::AppState;

/// Resolve base_dir for SDK relative paths: directory of the daemon config
/// file, falling back to ~/.actrail/actrail-kv/.
fn config_base_dir() -> Result<PathBuf, String> {
    let path = std::env::var("RAM_A_KV_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/etc/ram-a-kv/config.toml"));
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            return Ok(parent.to_path_buf());
        }
    }
    dirs::home_dir()
        .map(|h| h.join(".actrail").join("actrail-kv"))
        .ok_or_else(|| "could not locate home directory for actrail base_dir".to_owned())
}

/// Resolve a relative output path against base_dir; absolute paths pass through.
fn resolve_output(base_dir: &Path, output: &str) -> PathBuf {
    let p = Path::new(output);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base_dir.join(p)
    }
}

// ---------------------------------------------------------------------------
// POST /actrail/analyze
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct AnalyzeRequest {
    #[serde(default)]
    session_keys: Option<Vec<String>>,
    #[serde(default)]
    time_ranges: Option<Vec<TimeRangeInput>>,
    #[serde(default)]
    last_n: Option<usize>,
    #[serde(default)]
    top_k: Option<usize>,
    #[serde(default)]
    comparison_window_seconds: Option<u64>,
    #[serde(default)]
    output: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TimeRangeInput {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    until: Option<String>,
}

pub async fn handle_analyze(
    State(state): State<std::sync::Arc<AppState>>,
    Json(req): Json<AnalyzeRequest>,
) -> Json<EventResponse> {
    let lib = match &state.actrail_analyzer_lib {
        Some(lib) if !lib.trim().is_empty() => lib.clone(),
        _ => {
            return Json(EventResponse::error(
                Some("actrail_analyze"),
                "actrail_analyzer_lib not configured",
            ))
        }
    };

    let base_dir = match config_base_dir() {
        Ok(d) => d,
        Err(e) => return Json(EventResponse::error(Some("actrail_analyze"), &e)),
    };

    let selection = Selection {
        session_keys: req.session_keys,
        time_ranges: req.time_ranges.map(|ranges| {
            ranges
                .into_iter()
                .map(|r| TimeRange {
                    from: r.from,
                    until: r.until,
                })
                .collect()
        }),
        last_n: req
            .last_n
            .and_then(std::num::NonZeroUsize::new)
            .map(RecordLimit::Last)
            .unwrap_or_default(),
    };

    let output_path = req
        .output
        .as_deref()
        .map(|o| resolve_output(&base_dir, o))
        .unwrap_or_else(|| base_dir.join("analysis.json"));

    let overrides = build_analyzer_overrides(req.top_k, req.comparison_window_seconds);

    let result = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let mut analyzer = AnalyzerFactory::create(SdkConfig {
            library_path: lib.into(),
            base_dir: base_dir.clone(),
            config_toml: None,
            overrides,
        })
        .map_err(|e| format!("failed to load analyzer SDK: {e}"))?;

        let analysis = analyzer
            .analyze(&selection)
            .map_err(|e| format!("analysis failed: {e}"))?;
        analyzer.close().ok();

        analysis
            .export(&output_path)
            .map_err(|e| format!("failed to write analysis output: {e}"))?;

        Ok(json!({ "output": output_path.display().to_string() }))
    })
    .await;

    match result {
        Ok(Ok(data)) => Json(EventResponse::ok("actrail_analyze", data)),
        Ok(Err(e)) => Json(EventResponse::error(Some("actrail_analyze"), &e)),
        Err(e) => Json(EventResponse::error(
            Some("actrail_analyze"),
            &format!("analysis task failed: {e}"),
        )),
    }
}

fn build_analyzer_overrides(
    top_k: Option<usize>,
    comparison_window_seconds: Option<u64>,
) -> SdkOverrides {
    use actrail_kv_sdk::{AnalyzeConfigOverrides, ProjectionConfigOverrides};
    let options = AnalyzeConfigOverrides {
        top_k,
        projection: comparison_window_seconds.map(|secs| ProjectionConfigOverrides {
            comparison_window_seconds: Some(secs),
            ..Default::default()
        }),
        ..Default::default()
    };
    SdkOverrides {
        analyzer: Some(AnalyzerSettingsOverrides {
            options: Some(options),
            ..Default::default()
        }),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// POST /actrail/report
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ReportRequest {
    /// Path to analysis.json (from /actrail/analyze output).
    analysis: String,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    output: Option<String>,
}

#[derive(Debug, Serialize)]
struct ReportEntry {
    format: String,
    output: String,
}

pub async fn handle_report(
    State(_state): State<std::sync::Arc<AppState>>,
    Json(req): Json<ReportRequest>,
) -> Json<EventResponse> {
    let base_dir = match config_base_dir() {
        Ok(d) => d,
        Err(e) => return Json(EventResponse::error(Some("actrail_report"), &e)),
    };

    let input_path = resolve_output(&base_dir, &req.analysis);

    // Determine format: "html" / "md" / both (None).
    let format = match req.format.as_deref() {
        Some("html") => Some(actrail_kv_reporter::ReportFormat::Html),
        Some("md") => Some(actrail_kv_reporter::ReportFormat::Markdown),
        Some(other) => {
            return Json(EventResponse::error(
                Some("actrail_report"),
                &format!("invalid format '{other}', expected 'html' or 'md'"),
            ))
        }
        None => None,
    };

    // Resolve output path(s).
    let output_base = req
        .output
        .as_deref()
        .map(|o| resolve_output(&base_dir, o))
        .unwrap_or_else(|| base_dir.join("report"));

    let format_for_path = format;
    let result = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let paths = actrail_kv_reporter::report_paths(&output_base, format_for_path);
        let entries: Vec<ReportEntry> = paths
            .iter()
            .map(|(fmt, path)| ReportEntry {
                format: fmt.name().to_string(),
                output: path.display().to_string(),
            })
            .collect();

        // write_reports reads analysis.json and writes the report file(s).
        actrail_kv_reporter::write_reports(&input_path, &output_base, format_for_path)
            .map_err(|e| format!("failed to generate report: {e}"))?;

        Ok(json!({ "outputs": entries }))
    })
    .await;

    match result {
        Ok(Ok(data)) => Json(EventResponse::ok("actrail_report", data)),
        Ok(Err(e)) => Json(EventResponse::error(Some("actrail_report"), &e)),
        Err(e) => Json(EventResponse::error(
            Some("actrail_report"),
            &format!("report task failed: {e}"),
        )),
    }
}

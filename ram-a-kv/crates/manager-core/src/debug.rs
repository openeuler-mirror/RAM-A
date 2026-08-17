// Debug file writer.
// When debug_enabled=true, each inference turn writes a JSON file recording chunk
// hashes, LLM messages, and timing info.

use std::path::Path;

use crate::{KvCacheConfig, KvCacheError};

// Debug context: LLM conversation messages plus inference timing.
// Derives Default so callers can write a debug file even when the request did
// not carry a debug_context field (messages empty, timing all-null).
#[derive(Default)]
pub struct DebugContext {
    // LLM message list (full conversation history).
    pub messages: Vec<serde_json::Value>,
    // Inference timing info.
    pub timing: TimingInfo,
}

// Inference timing: per-stage durations. All fields optional; defaults to None.
#[derive(Default)]
pub struct TimingInfo {
    // TTFT (Time To First Token): latency to first token, in milliseconds.
    pub ttft_ms: Option<u64>,
    // Total turn inference latency, in milliseconds.
    pub total_time_ms: Option<u64>,
    // TPOT (Time Per Output Token): average per-token generation latency, in milliseconds.
    pub tpot_ms: Option<u64>,
}

// Write a debug file under `config.debug_dir`.
// Filename: `kvcache_debug_{sanitized_session_id}_{turn_count}.json`.
// Contents: session_id, turn_count, chunk_hashes, messages, timing.
//
// SECURITY: `session_id` comes from the HTTP request body, so it is treated as
// untrusted input. Only characters in `[A-Za-z0-9_-]` are kept; everything else
// is replaced with `_`. This blocks path-traversal attempts like
// `../../etc/hostname` or absolute paths that would otherwise escape
// `config.debug_dir` and write JSON to arbitrary locations.
pub fn write_debug_file(
    config: &KvCacheConfig,
    session_id: &str,
    turn_count: u32,
    chunk_hashes: &[String],
    debug_context: &DebugContext,
) -> crate::ManagerResult<()> {
    if !config.debug_enabled {
        return Ok(());
    }

    let dir = Path::new(&config.debug_dir);
    if !dir.exists() {
        std::fs::create_dir_all(dir).map_err(|e| KvCacheError::DebugWriteFailed {
            path: config.debug_dir.clone(),
            error: e.to_string(),
        })?;
    }

    let sanitized = sanitize_session_id(session_id);
    let filename = format!("kvcache_debug_{sanitized}_{turn_count}.json");
    let path = dir.join(&filename);

    let data = serde_json::json!({
        "session_id": session_id,
        "turn_count": turn_count,
        "chunk_hashes": chunk_hashes,
        "messages": debug_context.messages,
        "timing": {
            "ttft_ms": debug_context.timing.ttft_ms,
            "total_time_ms": debug_context.timing.total_time_ms,
            "tpot_ms": debug_context.timing.tpot_ms,
        },
    });

    let content = serde_json::to_string_pretty(&data).unwrap_or_default();
    std::fs::write(&path, content).map_err(|e| KvCacheError::DebugWriteFailed {
        path: path.display().to_string(),
        error: e.to_string(),
    })?;

    tracing::info!(path = %path.display(), "kvcache debug file written");
    Ok(())
}

// Replace any character outside `[A-Za-z0-9_-]` with `_` so the session_id can
// be safely embedded in a filename. Path separators (`/`, `\`), `..`, and
// NUL are all flattened to `_`, preventing escapes from `config.debug_dir`.
fn sanitize_session_id(session_id: &str) -> String {
    let mut out = String::with_capacity(session_id.len());
    for c in session_id.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    // Guard against empty/whitespace-only session_ids producing an empty
    // filename component (which would yield "kvcache_debug__1.json").
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_session_id;

    #[test]
    fn sanitizes_path_separators_and_dotdot() {
        assert_eq!(sanitize_session_id("safe_id"), "safe_id");
        assert_eq!(sanitize_session_id("a-b_c.123"), "a-b_c_123");
        // Path traversal attempts are flattened so they cannot escape the dir.
        assert_eq!(sanitize_session_id("../../etc/passwd"), "______etc_passwd");
        assert_eq!(sanitize_session_id("/abs/path"), "_abs_path");
        assert_eq!(sanitize_session_id("a\\b"), "a_b");
        assert_eq!(sanitize_session_id(""), "unknown");
        assert_eq!(sanitize_session_id("   "), "___");
    }
}

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

// Error types for the KV cache coordinator.

#[derive(Debug, thiserror::Error)]
pub enum KvCacheError {
    // Backend request failed (e.g. LMCache timeout or bad status code).
    #[error("backend request failed: {message}")]
    BackendFailed { message: String },

    // No session with the given session_id (e.g. accessed before creation).
    #[error("session '{session_id}' not found")]
    SessionNotFound { session_id: String },

    // Config parse error (e.g. invalid TOML).
    #[error("config error: {message}")]
    ConfigError { message: String },

    // Debug file write failed (e.g. missing dir or insufficient permissions).
    #[error("debug file write failed: {path}: {error}")]
    DebugWriteFailed { path: String, error: String },
}

// Unified result type: Ok(T) on success, Err(KvCacheError) otherwise.
// Generic so mutating methods can return structured outcomes (e.g. OperationOutcome).
pub type ManagerResult<T> = Result<T, KvCacheError>;

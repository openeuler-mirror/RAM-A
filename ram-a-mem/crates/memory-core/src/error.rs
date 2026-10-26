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

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("invalid input: {message}")]
    InvalidInput { message: String },
    #[error("embedding error: {message}")]
    Embedding { message: String },
    #[error("rerank error: {message}")]
    Rerank { message: String },
    #[error("extraction error: {message}")]
    Extraction { message: String },
    #[error("store error: {0}")]
    Store(#[from] std::io::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store backend error: {message}")]
    StoreBackend { message: String },
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type MemoryResult<T> = Result<T, MemoryError>;

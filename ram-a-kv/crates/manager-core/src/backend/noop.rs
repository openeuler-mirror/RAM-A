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

// NoopBackend: no-op backend for tests and development.
// Every operation returns success (HTTP 204) without sending real requests.
// Session tracking and refcount logic still run; only the actual cache ops are skipped.

use async_trait::async_trait;
use std::collections::HashMap;

use super::{BackendQueryResult, BackendResult, KvCacheBackend};

pub struct NoopBackend;

#[async_trait]
impl KvCacheBackend for NoopBackend {
    // Prefetch: return 204 (success, no content) without a real request.
    async fn prefetch(
        &self,
        _hashes: Vec<String>,
        _lookup_id: String,
        _session_id: String,
    ) -> BackendResult {
        BackendResult {
            status: 204,
            responded: true,
        }
    }

    // Evict: return 204 without a real request.
    async fn evict(&self, _hashes: Vec<String>) -> BackendResult {
        BackendResult {
            status: 204,
            responded: true,
        }
    }

    // Query: return an empty result without a real request.
    async fn query(&self, _hashes: Vec<String>) -> BackendQueryResult {
        BackendQueryResult {
            locations: HashMap::new(),
        }
    }
}

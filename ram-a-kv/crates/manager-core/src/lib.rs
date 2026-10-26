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

// manager-core: core logic library for the KV cache manager.
// Pure business logic: no HTTP service, SQLite persistence, or CLI.

pub mod backend;
pub mod config;
pub mod debug;
pub mod error;
pub mod manager;
pub mod map;

// Re-exported core types for the daemon layer.
pub use backend::{BackendQueryResult, BackendResult, KvCacheBackend};
pub use config::KvCacheConfig;
pub use error::{KvCacheError, ManagerResult};
pub use manager::KvCacheManager;
pub use map::KvCacheMap;

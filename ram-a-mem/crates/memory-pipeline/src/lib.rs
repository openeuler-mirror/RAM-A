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

//! Reusable long-term memory preparation pipeline.

mod atomic_file;

pub mod cache;
pub mod canonical;
pub mod client;
pub mod episode;
pub mod error;
pub mod extraction;
pub mod grounding;
pub mod models;
pub mod normalize;
pub mod pipeline;
pub mod validation;
pub mod window;
pub mod writer;

# Copyright (c) 2026 Huawei Technologies Co.,Ltd. All rights reserved.
#
# RAM-A is licensed under Mulan PSL v2.
# You can use this software according to the terms and conditions of the Mulan
# PSL v2.
# You may obtain a copy of Mulan PSL v2 at:
#         http://license.coscl.org.cn/MulanPSL2
# THIS SOFTWARE IS PROVIDED ON AN "AS IS" BASIS, WITHOUT WARRANTIES OF ANY
# KIND, EITHER EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO
# NON-INFRINGEMENT, MERCHANTABILITY OR FIT FOR A PARTICULAR PURPOSE.
# See the Mulan PSL v2 for more details.

"""Backend factory."""

from __future__ import annotations

from .base import BackendConfig, MemoryBackend
from .rama import RamaBackend


def create_backend(config: BackendConfig) -> MemoryBackend:
    if config.name == "RAM-A":
        return RamaBackend(config)
    raise ValueError(f"unknown backend: {config.name}. Supported backends: RAM-A")

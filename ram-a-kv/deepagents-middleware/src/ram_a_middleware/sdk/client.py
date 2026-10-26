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

"""Framework-neutral public clients for RAM-A."""

from .protocol import AsyncRamAProtocolClient, RamAProtocolClient


class RamAClient(RamAProtocolClient):
    """Send RAM-A events synchronously and return their acknowledgements."""


class AsyncRamAClient(AsyncRamAProtocolClient):
    """Send RAM-A events asynchronously and return their acknowledgements."""


__all__ = ["RamAClient", "AsyncRamAClient"]

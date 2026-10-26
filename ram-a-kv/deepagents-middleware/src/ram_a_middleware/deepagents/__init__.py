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

"""DeepAgents integration for RAM-A model lifecycle and agent start events.

The core SDK remains independent of LangChain and DeepAgents.  Import this
subpackage only in applications that use DeepAgents.
"""

from .events import (
    CallContext,
    CallResult,
    EventResult,
)
from .identity import Correlation, IdentityContext
from .middleware import AsyncRamAKvMiddleware, RamAKvMiddleware
from .serialization import json_safe

__all__ = [
    "AsyncRamAKvMiddleware",
    "CallContext",
    "CallResult",
    "Correlation",
    "EventResult",
    "IdentityContext",
    "RamAKvMiddleware",
    "json_safe",
]

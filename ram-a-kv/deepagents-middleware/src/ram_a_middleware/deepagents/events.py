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

"""Compatibility re-exports; protocol event types are owned by the SDK."""
from ram_a_middleware.sdk.events import (
    CallContext,
    CallResult,
    EventResult,
    EventStatus,
)

__all__ = [
    "CallContext", "CallResult", "EventResult", "EventStatus",
]

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

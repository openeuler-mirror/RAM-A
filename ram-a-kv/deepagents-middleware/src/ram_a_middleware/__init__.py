"""RAM-A Agent middleware package.

``ram_a_middleware.sdk`` is framework-neutral; ``ram_a_middleware.deepagents``
adapts the SDK to DeepAgents and LangChain.
"""
from .sdk import *  # noqa: F401,F403
from .sdk import __all__ as _sdk_all

__version__ = "0.3.0"
__all__ = list(_sdk_all)

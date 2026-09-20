"""Framework-neutral public clients for RAM-A."""

from .protocol import AsyncRamAProtocolClient, RamAProtocolClient


class RamAClient(RamAProtocolClient):
    """Send RAM-A events synchronously and return their acknowledgements."""


class AsyncRamAClient(AsyncRamAProtocolClient):
    """Send RAM-A events asynchronously and return their acknowledgements."""


__all__ = ["RamAClient", "AsyncRamAClient"]

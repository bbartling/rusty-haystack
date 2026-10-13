"""Public client API."""

from . import HDict, HGrid, _InputValue

class TlsConfig:
    """TLS client configuration for mTLS connections."""
    @staticmethod
    def from_files(
        cert_path: str, key_path: str, ca_path: str | None = None
    ) -> TlsConfig:
        """Load TLS certificates from file paths."""
        ...
    def __repr__(self) -> str: ...


class HaystackClient:
    """Haystack HTTP client with SCRAM SHA-256 authentication.

    All methods are synchronous (blocking). The GIL is released during
    network I/O so other Python threads can run.

    Examples:
        client = HaystackClient.connect("http://localhost:8080", "admin", "pass")
        sites = client.read("site")
        client.close()
    """
    @staticmethod
    def connect(url: str, username: str, password: str) -> HaystackClient:
        """Connect to a Haystack server over HTTP with SCRAM auth."""
        ...
    @staticmethod
    def connect_tls(
        url: str,
        username: str,
        password: str,
        tls: TlsConfig,
    ) -> HaystackClient:
        """Connect with mutual TLS authentication."""
        ...
    def about(self) -> HGrid:
        """Query the server's about information."""
        ...
    def ops(self) -> HGrid:
        """Query supported operations."""
        ...
    def formats(self) -> HGrid:
        """Query supported wire formats."""
        ...
    def libs(self) -> HGrid:
        """Query loaded ontology libraries."""
        ...
    def read(self, filter: str, limit: int | None = None) -> HGrid:
        """Read entities matching a Haystack filter expression.

        Args:
            filter: Haystack filter (e.g., 'site', 'equip and siteRef==@s').
            limit: Maximum results (None for unlimited).
        """
        ...
    def read_by_ids(self, ids: list[str]) -> HGrid:
        """Read entities by their ref ID strings."""
        ...
    def nav(self, nav_id: str | None = None) -> HGrid:
        """Navigate the entity tree. None for root."""
        ...
    def defs(self, filter: str | None = None) -> HGrid:
        """Query ontology definitions with optional filter."""
        ...
    def watch_sub(self, ids: list[str], lease: str | None = None) -> HGrid:
        """Subscribe to entity watches. Lease: '1min', '1hr', etc."""
        ...
    def watch_poll(self, watch_id: str) -> HGrid:
        """Poll a watch for changed entities."""
        ...
    def watch_unsub(self, watch_id: str, ids: list[str] | None = None) -> HGrid:
        """Unsubscribe from a watch. ids=None closes entire watch."""
        ...
    def point_write(self, id: str, level: int, val: _InputValue) -> HGrid:
        """Write a value to a writable point at a priority level (1-17)."""
        ...
    def his_read(self, id: str, range: str) -> HGrid:
        """Read historical data. Range: 'today', '2024-01-01,2024-01-31', etc."""
        ...
    def his_write(self, id: str, items: list[HDict]) -> HGrid:
        """Write historical data. Items are HDict rows with 'ts' and 'val' tags."""
        ...
    def invoke_action(self, id: str, action: str, args: HDict | None = None) -> HGrid:
        """Invoke a named action on an entity."""
        ...
    def specs(self, lib: str | None = None) -> HGrid:
        """Query ontology specs, optionally filtered by library."""
        ...
    def spec(self, qname: str) -> HGrid:
        """Get a single spec by qualified name."""
        ...
    def load_lib(self, name: str, source: str) -> HGrid:
        """Load an ontology library from Trio source."""
        ...
    def unload_lib(self, name: str) -> HGrid:
        """Unload an ontology library by name."""
        ...
    def export_lib(self, name: str) -> HGrid:
        """Export an ontology library as Trio source."""
        ...
    def validate(self, entities: list[HDict]) -> HGrid:
        """Validate entities against the server's ontology."""
        ...
    def call(self, op: str, req: HGrid) -> HGrid:
        """Send a raw op request. Returns the response grid."""
        ...
    def close(self) -> None:
        """Close the client connection."""
        ...
    def __repr__(self) -> str: ...


class WsClient:
    """Haystack WebSocket client with SCRAM authentication.

    Connects via HTTP for auth, then upgrades to WebSocket for operations.
    Ideal for long-lived connections and watch subscriptions.
    """
    @staticmethod
    def connect(
        url: str, ws_url: str, username: str, password: str
    ) -> WsClient:
        """Connect via HTTP for auth, then upgrade to WebSocket."""
        ...
    def about(self) -> HGrid:
        """Query server about information over WebSocket."""
        ...
    def ops(self) -> HGrid:
        """Query supported operations over WebSocket."""
        ...
    def read(self, filter: str, limit: int | None = None) -> HGrid:
        """Read entities matching a filter over WebSocket."""
        ...
    def read_by_ids(self, ids: list[str]) -> HGrid:
        """Read entities by ID over WebSocket."""
        ...
    def nav(self, nav_id: str | None = None) -> HGrid:
        """Navigate entity tree over WebSocket."""
        ...
    def watch_sub(self, ids: list[str], lease: str | None = None) -> HGrid:
        """Subscribe to entity watches over WebSocket."""
        ...
    def watch_poll(self, watch_id: str) -> HGrid:
        """Poll a watch over WebSocket."""
        ...
    def his_read(self, id: str, range: str) -> HGrid:
        """Read historical data over WebSocket."""
        ...
    def point_write(self, id: str, level: int, val: _InputValue) -> HGrid:
        """Write to a writable point over WebSocket."""
        ...
    def call(self, op: str, req: HGrid) -> HGrid:
        """Send a raw op request over WebSocket."""
        ...
    def close(self) -> None:
        """Close the WebSocket connection."""
        ...
    def __repr__(self) -> str: ...

"""Public server API."""

from .graph import SharedGraph
from .ontology import DefNamespace

class AuthManager:
    """SCRAM SHA-256 authentication manager for the server.

    Passing one to HaystackServer.with_auth consumes it. Every method on a
    consumed AuthManager raises RuntimeError, except repr(), which reports
    "AuthManager(consumed)".
    """
    @staticmethod
    def empty() -> AuthManager:
        """Create a disabled (no-auth) manager."""
        ...
    @staticmethod
    def from_toml(path: str) -> AuthManager:
        """Load auth configuration from a TOML file path."""
        ...
    @staticmethod
    def from_toml_str(content: str) -> AuthManager:
        """Load auth configuration from a TOML string."""
        ...
    def is_enabled(self) -> bool:
        """Whether authentication is enabled (has users configured)."""
        ...
    def __repr__(self) -> str: ...


class HisStore:
    """In-memory history storage for time-series point data."""
    def __init__(self) -> None: ...
    def len(self, id: str) -> int:
        """Number of history records for a given entity ID."""
        ...
    def __repr__(self) -> str: ...


class HaystackServer:
    """Embedded Haystack HTTP API server with builder-pattern configuration.

    Note: with_auth consumes its argument. The source AuthManager is left
    poisoned: every later call on it raises RuntimeError. with_namespace does
    not consume — it copies.

    Examples:
        server = HaystackServer(SharedGraph())
        server.with_auth(auth)
        server.port(8080)
        server.run()  # blocks
    """
    def __init__(self, graph: SharedGraph) -> None: ...
    def with_namespace(self, ns: DefNamespace) -> None:
        """Set the ontology namespace for validation and spec management.

        The namespace is copied into the server, so ns stays usable. The server
        keeps its own mutable copy because the lib load/unload endpoints mutate
        it; later changes on either side do not affect the other.
        """
        ...
    def with_auth(self, auth: AuthManager) -> None:
        """Set the auth manager, consuming it.

        `auth` is unusable afterwards and every later call on it raises
        RuntimeError. An AuthManager holds live SCRAM state — in-flight
        handshakes, issued tokens, and the server secret used to derive
        anti-enumeration challenges — so it moves rather than being shared or
        copied. Configure it fully before calling this.

        Raises:
            RuntimeError: if `auth` was already given to a server.
        """
        ...
    def port(self, port: int) -> None:
        """Set the HTTP listen port (default 8080)."""
        ...
    def host(self, host: str) -> None:
        """Set the HTTP listen host (default '0.0.0.0')."""
        ...
    def run(self) -> None:
        """Start the server, blocking the calling thread. Releases the GIL."""
        ...
    def run_background(self) -> None:
        """Start the server in a background thread. Returns immediately."""
        ...
    def bg_error(self) -> str | None:
        """Retrieve the background server error message, if any."""
        ...
    def __repr__(self) -> str: ...

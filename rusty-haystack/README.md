# rusty-haystack

Fast Python bindings for [Project Haystack](https://project-haystack.org) powered by Rust.

Provides types, codecs (Zinc/Trio/JSON/JSON v3/CSV), filters, an in-memory entity graph, ontology validation, and Xeto support — all backed by a native Rust core for speed and correctness.

## Installation

```sh
pip install rusty-haystack
```

Requires Python 3.11+. Pre-built wheels are available for Linux and macOS (x86_64 and aarch64).

## Quick Start

```python
import rusty_haystack as rh

# Create an entity
entity = rh.HDict({
    "id": rh.Ref("site-1", "Demo Site"),
    "site": rh.Marker(),
    "dis": "Demo Site",
    "area": rh.Number(5000, "ft²"),
    "geoCoord": rh.Coord(40.7128, -74.0060),
})

# Build a graph and query it
graph = rh.EntityGraph()
graph.add(entity)
results = graph.read("site")

# Encode/decode across formats
zinc_str = rh.encode_grid("text/zinc", results)
json_str = rh.encode_grid("application/json", results)
decoded = rh.decode_grid("text/zinc", zinc_str)

# Validate against the standard ontology
ns = rh.DefNamespace.load_standard()
issues = ns.validate_entity(entity)
```

## Features

- **Kind types** — Marker, NA, Remove, Number (with units), Ref, Uri, Symbol, XStr, Coord, HDateTime, HDict, HGrid, HList, plus exact Int, Float, NoneValue, Buf and Nominal wrappers
- **Codecs** — Zinc, Trio, JSON, JSON v3, CSV encoding and decoding
- **Filters** — Parse and evaluate Haystack filter expressions against entities
- **EntityGraph** — In-memory graph with CRUD, filter queries, and ref traversal
- **Ontology** — DefNamespace with `is_a`, `fits`, `validate_entity`, subtype/supertype queries
- **Xeto** — Load/unload Xeto libraries, inspect specs and slots, export to Xeto source

## Codecs

Supported MIME types: `"text/zinc"`, `"text/trio"`, `"application/json"`, `"application/json;v=3"`, `"text/csv"`.

```python
# Grid encode/decode
zinc_str = rh.encode_grid("text/zinc", grid)
grid = rh.decode_grid("text/zinc", zinc_str)

# Scalar encode/decode
s = rh.encode_scalar("text/zinc", rh.Number(72.5, "°F"))
v = rh.decode_scalar("text/zinc", s)

# Project-owned typed payload v1 preserves rich variants and exact float bits.
typed = rh.HDict({"count": rh.Int(2**53 + 1), "sample": rh.Float.from_bits(1)})
payload = rh.encode_typed_value(typed)
assert rh.decode_typed_value(payload) == typed
```

Plain Python integers still become Number, but must fit signed 64-bit and be
exactly representable as binary64; use Int when retaining integer identity or
precision matters. Number and Coord are immutable and use representation
equality, including distinct signed zero. H4 codecs reject rich values recursively.

HDateTime retains exact nanoseconds with a fixed offset and opaque timezone label.
Its `dt()` method rejects precision loss, leap seconds and out-of-range local
calendars. Calendar components and offsets accept concrete integers, including
bool, without invoking arbitrary numeric hooks. Native date/time conversion reads
stored fields without invoking subclass properties; time values require no
timezone and `fold=0`. Runtime checks remain
necessary for numeric ranges and temporal restrictions even with static typing.
The package ships root and submodule stubs for Python 3.11+.

## EntityGraph

```python
graph = rh.EntityGraph()
graph.add(rh.HDict({"id": rh.Ref("site-1"), "site": rh.Marker(), "dis": "HQ"}))
graph.add(rh.HDict({"id": rh.Ref("ahu-1"), "equip": rh.Marker(), "siteRef": rh.Ref("site-1")}))

results = graph.read("equip")       # filter query
graph.refs_from("ahu-1")            # -> ["site-1"]
graph.refs_to("site-1")             # -> ["ahu-1"]
```

## Ontology & Xeto

```python
ns = rh.DefNamespace.load_standard()

ns.is_a("ahu", "equip")            # True
ns.fits(entity, "site")            # True
ns.subtypes("equip")               # ["ahu", "vav", ...]
ns.validate_entity(entity)         # list of issue strings

# Load custom Xeto libraries
ns.load_xeto(source_text, "myLib")
spec = ns.get_spec("myLib::MyType")
```

## License

MIT

## Async HTTP client

`AsyncHaystackClient` owns an HTTP/SCRAM session and its native I/O resources. Its
constructor stores configuration without opening a connection or requiring an
asyncio loop. The first operation binds the exact running loop and OS thread;
using the owner from another loop or thread raises `RuntimeError`, even after
close. Create a fresh owner for another loop or after a terminal failure.

```python
import asyncio
from rusty_haystack import AsyncHaystackClient
from rusty_haystack.client import TlsConfig

async def read_sites(url: str, username: str, password: str):
    client = AsyncHaystackClient(url, username, password, timeout=30.0)
    async with client:
        deadline = asyncio.get_running_loop().time() + 5.0
        return await client.read("site", limit=100, deadline=deadline)
```

You can instead `await client.connect()` and use `await client.close()` in a
`finally` block. Keep the owner before connecting so failed or cancelled connect
can still be observed through `close`. Failed or cancelled connect starts
terminal cleanup. A cancelled read cancels that request; the open client remains
usable once its admission slot has drained.

`connect`, `read`, `close`, `__aenter__` and `__aexit__` return asyncio **Futures**.
Direct `await`, `asyncio.ensure_future`, `gather` and `wait_for` accept them. APIs
requiring coroutine objects, including `create_task` and `TaskGroup.create_task`,
need an explicit wrapper:

```python
async def read_one(client):
    return await client.read("site", limit=1)

# Inside a running loop:
# task = asyncio.create_task(read_one(client))
```

The default operation budget is 30 seconds. Constructor `timeout` must be positive,
finite and representable. An optional absolute `deadline` uses the bound loop's
`time()` clock and can **shorten** that budget. The clock is converted once at the
method call, so queueing, requests and body collection share one deadline. Deadline
expiry raises builtin `TimeoutError`; asyncio cancellation remains
`CancelledError`. Native authentication, transport, protocol, overload and state
failures raise `rusty_haystack.ClientError` with sanitized diagnostics. Local
argument/conversion exceptions keep their Python types.

Reads accept a positive limit of at most 1000 (default 100), up to 8192 UTF-8 bytes
of filter text, and at most 1 MiB of response bytes before decoding. Declared and
streamed response sizes are checked. Responses exceeding the requested row limit
are rejected, never truncated. At most 16 native operations, including their
result-conversion guards, are admitted at once. Overload is rejected immediately;
there is no unbounded pending-operation queue. This small API has no arbitrary raw
operation, watches or complete H5 operation family.

`state` reports `new`, `connecting`, `open`, `closing` or `closed`; `closed` is true
only after actual teardown. Close seals admission immediately, cancels requests,
destroys the owned native runtime and joins its thread, then drains operation and
result-conversion guards. This includes already-started DNS/SCRAM blocking work
that cannot be preempted by cancelling its request. Repeated close observes the
same cleanup. A cancelled or timed-out close waiter does not stop cleanup and is
not a graceful-close receipt. A valid expired close deadline still starts cleanup
and raises `TimeoutError` unless the owner was already closed. Invalid deadlines
raise before starting cleanup. If context-body failure and cleanup failure both
occur, the original body exception propagates, with a cleanup note when possible.

Dropping the Python owner requests cleanup without blocking or making Python
callbacks; it does not prove graceful completion. The process-wide asyncio bridge
and legacy synchronous runtimes have separate lifetimes. Successful read conversion
uses guarded Python attachment, and late bridge notification is skipped if Python
cannot be attached. An isolated native subprocess test covers a completed read
result before Python initialization, plus conversion after initialization; it does
not finalize a live test interpreter or prove every finalization race safe. Selected
host subprocess tests cover exit with unfinished work, but only awaited successful
close is the native teardown receipt. The synchronous `HaystackClient` and
`WsClient` remain blocking APIs with their existing ownership behavior.

TLS uses verified system trust plus optional custom trust and identity. Pass
`tls=TlsConfig.with_ca(ca_pem_bytes)` for an additional CA, or
`TlsConfig.from_files(cert_path, key_path, ca_path)` for mutual TLS. There is no
insecure-verification option in the async API; redirects and automatic retries
remain disabled. HTTP authentication and reads reuse the same TLS configuration.

The selected runtime evidence is CPython 3.12 on macOS arm64; installed stub checks
target Python 3.11. Broader platform and free-threaded qualification is separate.
The independent TLS tests require an OpenSSL CLI and generate temporary seven-day
certificates so platform certificate policies and stale fixtures are respected.

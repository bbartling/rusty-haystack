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

## Controlled embedded applications

`ApplicationBuilder` starts an in-memory application using the same native
application lifecycle and bounded read service as Rust embedding. `start()` is
synchronous, requires a running asyncio loop, consumes the builder once, and
returns the sole `ApplicationOwner` immediately. Configuration is fixed at that
point. Await `ready()` for the actual `(host, port)` after initialization, or use
the owner as an async context manager:

```python
import asyncio
from rusty_haystack import ApplicationBuilder, HDict, Marker, Ref, SharedGraph

async def main():
    graph = SharedGraph()
    graph.add(HDict({"id": Ref("a"), "site": Marker()}))
    app = ApplicationBuilder(graph, port=0).start()
    async with app:
        address = await app.ready()
        view = app.handle()
        page = await view.read("site", limit=100)
        print(address, page.meta()["complete"], len(page))
    assert app.closed
    assert (await view.terminated()).close_error is None
    assert graph.contains("a")  # the caller's graph remains usable

asyncio.run(main())
```

The default listener binds `127.0.0.1` and ephemeral port `0`. `host` accepts
numeric IPv4/IPv6 addresses, avoiding separate hostname-resolution work. An
omitted graph creates an empty in-memory graph. A supplied `SharedGraph` remains
shared mutable state owned by its caller; configuration freeze does not freeze
entity contents. Closing an application releases its listener and admitted work,
and borrows the process-wide bridge runtime throughout. Other applications and
clients continue using that bridge after closure.

Direct Python reads use explicit trusted-embedding authority. HTTP entity and
catalog reads require an authenticated principal with read/admin permission.
Pass a fully configured `rusty_haystack.server.AuthManager` as `auth=` to enable
SCRAM. The manager is consumed only when `start()` validates the loop and claims
the configuration; invalid construction or a start without a running loop leaves
it usable. With `auth=None`, public HTTP protocol metadata remains available,
but entity data is denied. Remote requests cannot obtain trusted-embedding
authority by supplying a subject or header.

This profile selects scoped reads and the existing native read/catalog/system
HTTP routes. It selects no mutation, history, subscription, watch, custom-router,
database or Python-callback providers. Capability discovery derives from that
native selection. The embedded/HTTP read evidence covers a shared in-memory read
service and lifecycle; it does not establish parity with every legacy CLI
capability/default or qualify PostgreSQL. Legacy `HaystackServer.run()` and
`run_background()` retain their existing behavior and are outside this profile.

`ApplicationHandle` is a borrowed readiness/read/status/termination view with no
`close` method. Dropping it has no shutdown effect. Keeping it does not keep the
owner alive; owner closure makes its retained handles terminal to new reads.
`read(filter, limit=100, cursor=None, deadline=None)` returns an `HGrid` with native
`complete` and optional `cursor` metadata. Pass the cursor with the same query and
page size to continue. Native cursors remain scoped to the service and principal;
this API does not promise cross-principal HTTP/Python cursor exchange.

Reads allow 1–1000 rows, filter/cursor strings up to 8192 UTF-8 bytes each, and
the native conservative 1 MiB output budget. The shared service retains its other
finite defaults, including eight executing workers, sixteen queue slots and a
five-second read budget. At most sixteen Python read/conversion observations are
pending per application across all its views. Native workers remain tracked if a
Python observer is cancelled; a parent work guard also covers Python conversion,
including a conversion error, before termination can complete.

The owner and its handles bind to the exact loop and OS thread that called
`start()`. `ready`, `read`, `close`, `terminated` and `__aexit__` return Futures
and support the same task patterns as `AsyncHaystackClient` above. `__aenter__`
returns a coroutine-compatible awaitable so cancellation is handled in the task
performing context entry. Its readiness observation begins when awaited. Await
it directly through `async with`; a separately scheduled entry task
has its own result handoff and requires its caller to retain shutdown ownership.
An optional absolute `deadline` uses that loop's
`time()` clock, is converted once, and can shorten the thirty-second observation
budget; a read additionally retains the native five-second budget. Expired reads
are rejected before admission. Invalid/nonfinite deadlines fail before mutation.
Ordinary cancelled readiness observers leave the application running. Failed or
cancelled `__aenter__` requests shutdown, including cancellation after native
readiness but before its awaiting task resumes. Disposing of an unstarted entry
coroutine or cancelling it before its first poll also requests shutdown. Retain
the owner before awaiting so cleanup can always be observed through `close()`.

`close()` seals native admission before returning its Future. Cleanup continues
independently of cancelled/expired waiters, and repeated close observes the same
result. A valid already-expired close still seals and raises `TimeoutError` unless
termination was already complete. Successful close waits for actual native
termination: owned accepted sockets, workers, conversion guards and resource
cleanup have finished. The native policy permits one second of drain followed by
four seconds of cooperative stop; exceeding that allowance records a failure and
retains cleanup until it actually finishes. An observation deadline can return
before that happens without claiming completion.

`state` preserves native `starting`, `running`, `closing`, `closed` and `failed`.
`failed` alone is not a cleanup receipt. `closed` means termination has actually
completed, including a failed outcome. `terminated()` returns an immutable
`ApplicationTermination` with `close_error`, `cleanup_errors` and `drain_expired`
(the latter is `None` when native close failed). An earlier native timeout remains
in this report even after cleanup succeeds; repeated close raises the retained
`ApplicationError`. Observation/read deadlines raise built-in `TimeoutError`,
asyncio cancellation remains `CancelledError`, and local argument/conversion
errors keep their Python types. Context exit preserves an original body exception
if cleanup or cleanup-note annotation also fails.

Owner destruction requests native cleanup without blocking or Python callbacks;
await close/termination for evidence. The existing guarded attachment and best
effort interpreter-finalization limits still apply. Supervised unfinished exits
and installed consumers qualify the selected macOS arm64/CPython 3.12 environment;
Python 3.11 is the static typing floor, not an additional runtime qualification.

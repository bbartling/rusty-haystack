# Arrow, Parquet, DataFusion, and Python update plan

Status: proposal; none of the new APIs below are implemented by this document.
Prepared October 6, 2026 from the local repositories and official documentation.
Companion: [Cursor implementation prompt](cursor-arrow-datafusion-prompt.md).

## Recommended outcome

Make rusty-haystack useful as a Rust and Python analytics connector:

1. Read history from an existing Haystack server.
2. Convert it to Arrow record batches in Rust.
3. Stream those batches to Python or a Parquet export.
4. Query snapshots with DataFusion immediately.
5. Add an optional live DataFusion table provider that turns SQL point/time predicates into remote history requests.

PostgreSQL/TimescaleDB is a separate server-storage track. It should use the same history semantics, but installing or operating PostgreSQL must not be required for the analytics workflow. Arrow is the memory format, Parquet is the file format, and DataFusion is the query engine; an export pipeline does not by itself provide a transactional database.

This matches Ben's priority and Justin's request to keep DataFusion optional. It also leaves room for SeleneDB or another graph backend without inventing an interface for a project whose implementation has not been inspected.

## What exists today

Inspection anchors:

| Repository | Branch and inspected commit | Package baseline |
|---|---|---|
| rusty-haystack | local `dev`, `40f6e0c4e052215e3e4bededbbf94a075d634df7` | 0.9.0; Rust edition 2024; MSRV 1.97 |
| rusty-bacnet | local `main`, `53bd87d2f0516580d26938a3a3b385e791b756f1` | 0.12.0; pinned toolchain 1.99.0; MSRV 1.93 |

These are checkout findings, not assertions about unretrieved upstream changes.

| Area | Haystack finding | Consequence |
|---|---|---|
| Python bindings | Existing PyO3 wrappers for types, codecs, graph, ontology, auth, HTTP/WS clients, and embedded server | Improve the current package; do not rebuild it from scratch |
| PyO3 | Manifest uses 0.29; lockfile contains 0.29.0 | Justin's suggested major upgrade has already happened in this checkout |
| Typing | `rusty_haystack.pyi` already exists; no `py.typed` file found and no explicit stub inclusion in maturin config | Verify actual wheel contents and typing coverage before claiming typing is absent or complete |
| Python network API | Synchronous methods use `py.detach()` around Tokio `block_on` | Useful foundation, but they still block an asyncio caller's event-loop thread |
| Client configuration | Rust exposes `ClientConfig`, Basic/SCRAM selection, timeout, wire format, and TLS options; Python lacks corresponding configuration bindings | Important parity gap for Niagara and large reads |
| History | Rust/Python `his_read(id, range)` returns an `HGrid` for one point | No existing analytics batch/stream API |
| HTTP response | `HttpTransport::call` reads `response.text()`, then decodes an entire grid | Chunking a returned grid afterward does not bound network/decode memory |
| Server history | `HistoryProvider` is injectable through `HaystackServer::with_history_provider` | Justin's storage breakpoint is real |
| Provider limitations | Read returns `Vec<HisItem>`; write returns `()`; neither returns a storage error | Durable adapters need a fallible contract; large reads need a stream/cursor |
| Metadata | Server directly owns `SharedGraph` | The history trait is not a general entity/graph persistence abstraction |
| Arrow / Parquet / DataFusion / SQL | No implementation or dependency found in the inspected manifests and source | These are new optional integrations |

Source map for review: `rusty-haystack/src/{lib,client,server,data}.rs`, `rusty-haystack/{Cargo.toml,pyproject.toml,rusty_haystack.pyi}`, `haystack-client/src/{client,config}.rs`, `haystack-client/src/transport/http.rs`, `haystack-server/src/{his_provider,his_store,state,app}.rs`, `haystack-server/src/ops/his.rs`, and `haystack-core/src/codecs/{mod.rs,zinc/}`.

BACnet's reference implementation is in `crates/rusty-bacnet/src/client/`, `src/py_async/`, its `.pyi`, `py.typed`, `pyproject.toml`, and `docs/python-api.md`. Its useful conventions are awaitable I/O, async context management, typed results, cancellation behavior, and installed-package verification. Its custom Tokio/asyncio bridge includes interpreter-exit handling; copying just the Future creation code would omit essential lifecycle behavior.

### Existing Python documentation drift

The current `docs/python.md` says Python 3.8+, whereas packaging requires 3.11+. Some examples call top-level `rh.HaystackClient` / `rh.HaystackServer`, but registration puts these classes in the `client` / `server` submodules. The WebSocket example calls `HaystackClient.connect_ws`, while the wrapper exposes `WsClient.connect`. The mTLS example passes certificate-path keywords, but the wrapper accepts a `TlsConfig` object. Repair these as part of Python parity and test the documented examples against the installed wheel.

## History correctness comes before bulk export

The local server's history code has concrete gaps:

- `handle_read` uses request row zero and expects row-level `range`; there is no batch request handling.
- `parse_range` supports dates/today/yesterday, but not the advertised datetime forms.
- Date boundaries use UTC rather than point metadata.
- The end boundary is inclusive at 23:59:59, so fractional samples in the final second can be excluded.
- Read responses include `id` but omit `hisStart`/`hisEnd`, and label timestamps with `UTC` without retaining original timezone identity.
- `HisItem` retains a fixed-offset timestamp but not the Haystack timezone name.
- The in-memory store silently drops old data beyond 1,000,000 samples per series.
- Storage failures cannot propagate through the current trait.

These are source findings. Runtime qualification remains to be done.

The current [Haystack Ops specification](https://project-haystack.org/doc/docHaystack/Ops#hisRead) defines single and batch reads. Batch requests put `range` in grid metadata and IDs in rows; responses use `ts` plus `v0`, `v1`, etc., with point IDs in column metadata. Ranges use inclusive start/exclusive end. Date ranges follow point timezones; batch reads need compatible zones or an explicit response timezone.

Use that standard batch mode when supported. Existing third-party servers may only support single reads, so expose `auto`, `single`, and `batch` modes. In `auto`, use a documented capability result or a narrow cached probe. Auth errors, malformed data, and timeouts are not evidence that batch mode is unsupported.

### Does one request handle lots of history?

Yes, a large single-point read or a supported batch read can carry many samples. One request is not a guarantee of completeness, low memory use, or acceptable server latency.

Offer three explicit strategies:

| Strategy | Best use | Limitation |
|---|---|---|
| One large standard read | Server supports the requested span and response size | Requires incremental decode to keep client memory bounded |
| Standard batch read | Many points sharing a range | A wide sparse response can be expensive; limit points per request |
| Bounded point/time windows | Older or constrained servers; resumable exports | More requests; must handle boundaries, retries, and deduplication |

Inspect the [HTTP incomplete-data metadata](https://project-haystack.org/doc/docHaystack/HttpApi#incompleteData). An incomplete response must not be reported as a successful complete export. Fail explicitly or retry smaller windows using a configured strategy. No universal history paging token should be assumed.

Zinc already has header/row encoding helpers, but its decode API takes a complete string. Reuse the existing parser and scalar rules when adding incremental decoding; do not split arbitrary incoming chunks on newlines without handling quoted strings, nested values, and partial UTF-8. JSON buffering can remain an explicitly limited fallback until it receives a qualified incremental decoder.

## Architecture and dependency boundaries

Suggested package names are design proposals.

```text
Existing Haystack server
          |
    HTTP history source
          |
    shared history samples
          |
     Rust Arrow batches
          +------> Arrow C stream ------> Python Arrow tools
          +------> Parquet snapshots --> DataFusion Rust / Python
          +------> optional live DataFusion TableProvider

Embedded Haystack server
          |
   shared HistoryProvider
          +------> in-memory test/demo store
          +------> optional Postgres / TimescaleDB adapter
          +------> later Parquet archive or SeleneDB adapter
```

| Component | Responsibility | Dependency rule |
|---|---|---|
| Existing core/client/server | Protocol, entities, ordinary grid APIs | No DataFusion or Postgres requirement |
| Lightweight `haystack-history` module/crate if extraction is justified | Samples, range semantics, history errors, fallible stream contract | No Arrow/DataFusion/database dependency |
| `rusty-haystack-arrow` Rust crate | History-to-Arrow conversion; optional client integration and `parquet` feature | One Arrow version family; no DataFusion requirement |
| `rusty-haystack-datafusion` Rust adapter | Read-only remote history table provider | Depends on client/Arrow plus DataFusion explicitly |
| Base Python `rusty_haystack` | Existing API, new Arrow stream/export methods, optional async client | Native Arrow/Parquet features can be selected for wheel builds |
| Optional Python `rusty_haystack_datafusion` plugin | Export a live provider to the independently installed DataFusion package | Separate wheel keeps DataFusion out of the base native extension |
| Optional Postgres adapter | Durable history reads/writes and migrations | Backend-specific crate/feature |

Python extras install Python dependencies; they cannot enable Cargo features inside an already compiled wheel. An `[arrow]` extra can install PyArrow convenience support. An `[datafusion]` extra can install DataFusion and the separate provider plugin once published. Base wheels may ship native Arrow/Parquet functionality without making PyArrow a required import. Make compiled capabilities discoverable and test the shipped wheel profile.

Do not pass private Rust structs between independently compiled Python extensions. The provider plugin should construct its own native client from ordinary configuration data or use a separately specified stable interface.

## A stable analytics schema

Prefer a long history table: one actual sample per row. It avoids creating a new Arrow schema for each set of points and works with SQL joins.

Proposed v1 columns:

| Column | Arrow type | Meaning |
|---|---|---|
| `source` | Utf8, required | Caller-defined server/project identity; no credentials |
| `point_id` | Utf8, required | Canonical Ref value |
| `ts` | Timestamp(ns, UTC), required | Absolute sample instant |
| `tz` | Utf8, required | Returned Haystack timezone identity |
| `offset_seconds` | Int32, required | Returned sample UTC offset |
| `value_kind` | Utf8, required | Distinguishes number, bool, str, NA, Null, etc. |
| `value_num` | Float64, nullable | Numeric value without silently converting units |
| `value_bool` | Boolean, nullable | Boolean value |
| `value_str` | Utf8, nullable | String value |
| `unit` | Utf8, nullable | Unit actually attached to the sample number |
| `value_zinc` | Utf8, required | Canonical encoded value for faithful reconstruction |

This is a recommended contract, not a standardized Haystack Arrow schema. Include schema and codec version metadata. Preserve Ref display names, markers, NA, Remove, coordinates, extended strings, and supported nested values through the canonical value field. Never use Debug output as serialization.

Null and NA must remain distinguishable. Wide batch alignment gaps are not observations: do not invent null samples for missing cells. If an upstream batch representation cannot distinguish a real Null sample from an alignment gap, document that information limit and allow a single-point mode for faithful retrieval.

UTC normalization is useful for querying, but keep timezone identity and offsets. Reject out-of-range Arrow nanosecond timestamps rather than overflow or silently reduce precision. Validate fractional timestamps and DST transitions.

Export point metadata separately, keyed by `(source, point_id)`, with useful projected fields such as display name, site/equipment refs, unit, and configured timezone, plus a canonical entity representation. Keep snapshot identity in the manifest. Metadata is a snapshot of what was observed during export, not proof of what tags were attached at each historical instant.

Grouping only by `point_id` can merge unrelated servers. Averaging mixed units can produce meaningless results. Examples should group by source and unit, or explicitly convert compatible units through the existing unit library.

## Three DataFusion experiences

### 1. In-memory Arrow interoperability

Proposed API; requires implementation:

```python
from datafusion import SessionContext
from rusty_haystack.client import HaystackClient

client = HaystackClient.connect(url, username, password)
try:
    with client.history_batches(
        ids=["point-1"],
        start="2026-10-01T00:00:00Z",
        end="2026-10-02T00:00:00Z",
        source="building-a",
    ) as batches:
        frame = SessionContext().from_arrow(batches, name="history")
        frame.show()
finally:
    client.close()
```

The existing [DataFusion `from_arrow` interface](https://datafusion.apache.org/python/autoapi/datafusion/context/index.html#datafusion.context.SessionContext.from_arrow) accepts Arrow export protocols. Implement `__arrow_c_stream__` on the new batch reader using the [Arrow PyCapsule interface](https://arrow.apache.org/docs/format/CDataInterface/PyCapsuleInterface.html). This removes per-sample Python object conversion after Rust builds Arrow buffers. It does not make Zinc/JSON parsing zero-copy or prove that DataFusion imports a stream lazily. Qualify the selected consumer's ingestion behavior; use this path for bounded datasets.

### 2. Parquet snapshots: first useful deliverable

Proposed Haystack export API; DataFusion calls shown are existing APIs:

```python
from datafusion import SessionContext
from rusty_haystack.client import HaystackClient

client = HaystackClient.connect(url, username, password)
try:
    with client.history_batches(
        ids=["point-1", "point-2"],
        start="2026-10-01T00:00:00Z",
        end="2026-10-02T00:00:00Z",
        source="building-a",
        request_mode="auto",
    ) as batches:
        summary = batches.write_parquet("history.parquet", compression="zstd")
finally:
    client.close()

ctx = SessionContext()
ctx.register_parquet("history", "history.parquet")
ctx.sql("""
    SELECT source, point_id, unit, AVG(value_num) AS mean_value
    FROM history
    WHERE value_kind = 'number'
    GROUP BY source, point_id, unit
""").show()
```

[DataFusion's Parquet integration](https://datafusion.apache.org/python/user-guide/io/parquet.html) also enables the same workflow in Rust with `SessionContext::register_parquet`. Implement the actual writer in Rust so both languages share schema and export behavior.

Use bounded Arrow batches and bounded Parquet row groups; [ArrowWriter](https://docs.rs/parquet/latest/parquet/arrow/arrow_writer/struct.ArrowWriter.html) buffers row-group state, so record-batch size alone is not a memory limit. Publish only after closing the writer successfully. Prefer a sibling staging file and atomic publication with an explicit overwrite policy.

Start with one valid file plus a manifest and metadata snapshot. Later add partitions by source and UTC date, with bounded point buckets if measured workloads justify them. Avoid creating one tiny file per point/window. Repeated exports should create immutable snapshots; they must not accidentally double-count data on directory scans. A resumable dataset needs a manifest of completed windows/files and a defined correction/deduplication policy.

### 3. Live SQL against Haystack

Proposed optional plugin:

```python
from datafusion import SessionContext
from rusty_haystack_datafusion import HaystackHistoryTable

provider = HaystackHistoryTable(
    url=url, username=username, password=password,
    source="building-a", ids=["point-1", "point-2"],
    start="2026-10-01T00:00:00Z", end="2026-10-02T00:00:00Z",
)
ctx = SessionContext()
ctx.register_table("remote_history", provider)
ctx.sql("""
    SELECT point_id, unit, AVG(value_num)
    FROM remote_history
    WHERE point_id = 'point-1'
    GROUP BY point_id, unit
""").show()
```

The Rust adapter implements a `TableProvider` and a physical execution stream. SQL point/time predicates can reduce remote requests; projections reduce constructed columns. Unsupported predicates remain for DataFusion to evaluate. Registration/planning should not download history; execution should pull bounded batches and release work when canceled. Start with explicit point IDs and a finite configured time envelope to make a full-table scan meaningful and bounded.

Expose the Python provider with [DataFusion's provider export protocol](https://datafusion.apache.org/python/user-guide/io/table_provider.html), rather than embedding an unrelated query engine behind a method named `sql`. Follow the [custom-provider execution and pushdown guidance](https://datafusion.apache.org/library-user-guide/custom-table-providers.html). SQL limits, ordering, and inexact predicates need explicit correctness tests before source optimizations are claimed.

This is the most complex analytics milestone. It combines remote I/O, streaming, query semantics, cancellation, and an independently versioned FFI boundary.

## Python and build modernization

Preserve existing synchronous APIs while adding a typed `AsyncHaystackClient` with awaitable I/O and `async with`, following BACnet's user-facing conventions. A sync batch iterator can support ordinary Arrow consumers; async acquisition/iteration needs a separate clear contract. Never block an asyncio loop with the current sync wrapper and call that native async support.

Verify wheels contain stubs and correct module paths; add typing smoke examples for ordinary clients, batch readers, and optional integrations. Include `ClientConfig` parity and the history-provider attachment path needed by Python server users.

[PyO3 0.29 free-threading guidance](https://pyo3.rs/v0.29.0/free-threading.html) says modules default to supporting free-threading from 0.28 onward. Missing `gil_used = false` is therefore not the defect here. Audit mutable classes, locks, callbacks, initialization, and shutdown; build and test CPython 3.14 and 3.14t artifacts separately. Keep existing Python 3.11+ support unless a deliberate project decision changes it.

Current Haystack CI runs Python tests on 3.12, and its release wheel matrix lists 3.11–3.13 on Linux/macOS. Extend actual build/test coverage before advertising 3.14, free-threading, or additional platforms.

Dependency candidates checked in official documentation, not compiled together during this review:

| Family | Candidate | Evidence / action |
|---|---|---|
| PyO3 | 0.29.x | Already used; select a current patch and verify |
| Python Arrow bridge | pyo3-arrow 0.19.x | [Compatibility table](https://docs.rs/pyo3-arrow/latest/pyo3_arrow/) pairs PyO3 0.29 with Arrow 59 |
| Arrow / Parquet | Consistent 59.x family | Check exact APIs and unify workspace versions |
| DataFusion / FFI | Compatible 55.x Rust family and qualified Python package | [DataFusion manifest](https://docs.rs/crate/datafusion/latest) lists Arrow 59.2; validate the Python FFI combination |
| Rust toolchain | 1.98.1 or 1.99.0, per project decision | Haystack CI currently pins 1.97.1; local stable is 1.98.1 |

These are a compatibility starting point, not instructions to install every newest release. Recheck registry metadata, MSRVs, and Python artifact availability at implementation time. A free-threaded rusty-haystack wheel does not establish that PyArrow or DataFusion's Python distribution supports the same interpreter.

## Separate PostgreSQL / TimescaleDB track

Upgrade the shared history contract first, then implement a backend that:

- returns explicit errors for reads/writes and streams large ordered reads;
- uses transactions and a defined replace-on-duplicate policy;
- identifies samples by project/source, point, and exact instant;
- preserves Haystack kinds and units, with indexed numeric projections;
- supports migrations, restart durability, connection pooling, and configured retention.

PostgreSQL `timestamptz` does not retain the original timezone name and has microsecond precision; preserve timezone identity separately and use an exact nanosecond representation if the history contract promises nanoseconds. See [PostgreSQL datetime types](https://www.postgresql.org/docs/current/datatype-datetime.html).

TimescaleDB can be an optional extension-specific layout, qualified independently. Normal Postgres users should not need TimescaleDB. Durable point metadata and graph relationships require their own design because `HistoryProvider` does not intercept graph operations.

A Parquet exporter is not a writable `HistoryProvider`. A future Parquet-backed archive would need manifests, write visibility, corrections, compaction, recovery, and retention semantics. Leave that distinction clear in product documentation.

## Suggested implementation issues and acceptance gates

These are local issue drafts, not filed GitHub issues.

| ID | Suggested title | Dependencies / scope | Acceptance gate |
|---|---|---|---|
| A0 | Qualify Python packaging and expose client configuration | Independent first PR | Installed stubs; valid docs imports/TLS/WS examples; configuration parity; baseline test evidence |
| A1 | Correct history ranges and implement standard batch reads | History foundation | Boundary/DST/fractional tests; response metadata; batch column IDs; bounded single fallback; incomplete-data failures |
| A2 | Add a shared Rust history-to-Arrow contract | A1 semantics; conversion can start earlier | Stable schema; kind/unit/timezone fidelity; no per-row Python conversion; no DataFusion dependency |
| A3 | Stream remote history to bounded Parquet snapshots | A1 + A2 | Incremental Zinc; memory ceilings; cancellation; incomplete/error handling; atomic publication; Rust/Python exports queryable in DataFusion |
| A4 | Expose typed Arrow readers and async Python history APIs | A0 + A2/A3 | Arrow C-stream ownership; async lifecycle; installed-wheel typing; 3.14 and 3.14t qualification |
| A5 | Add an optional live DataFusion history provider | A3 + A4 | Lazy execution; point/time pushdown and residual filters; FFI compatibility; request-count tests; bounded finite scans |
| S1 | Add fallible streaming history storage and Postgres adapter | Shared semantics; backend track | Failure propagation; transaction/idempotency tests; persistence across restarts; no backend requirement in default APIs |
| S2 | Define durable entity/graph storage and archive adapters | Separate design | Graph/metadata consistency contract; migration/compaction plan; SeleneDB API evidence |
| W1 | Review WebSocket history/streaming parity | Deferred transport work | Explicit operation support and lifecycle tests; analytics does not depend on this first |

First useful target: A0–A3, plus the narrow Python Arrow/export surface from A4. This should deliver external server → Parquet → DataFusion without a database migration or live provider. Complete the remaining A4/A5 work next. S1 can proceed separately after shared semantics stabilize.

A2 is moderate conversion work. A1/A3/A5 carry the substantial protocol, memory, and query-correctness effort. This is several bounded PRs, not just adding an Arrow dependency or applying a PyO3 version bump.

## Validation and review limits

Completed during this assessment: inspected both source checkouts, manifests, workflows, stubs, existing tests, and official upstream interfaces; parsed Haystack workspace manifests with `cargo +stable metadata --no-deps --offline --locked`. This checks manifest structure, not dependency compatibility or compilation.

No software implementation, remote-server benchmark, wheel build, Python runtime test, or database qualification was performed. The companion prompt defines the tests implementation must supply. Proposed APIs and dependency combinations must not be presented as working features until those gates pass.

For Justin's review, the useful decisions are: approve the long-table schema; decide whether base Python wheels ship Arrow/Parquet native features; settle support versions and module naming; and agree on the shared history contract before S1. The first analytics export does not depend on choosing a future graph database.

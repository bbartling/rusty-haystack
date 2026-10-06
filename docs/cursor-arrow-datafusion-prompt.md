# Cursor implementation prompt: Haystack → Arrow → Parquet → DataFusion

Use this prompt in the rusty-haystack repository. Read the [human plan](arrow-datafusion-update-plan.md) first. This document specifies future work; it is not evidence that any proposed API is currently available.

## Mission and priorities

Implement a shared Rust history analytics pipeline and ergonomic Python bindings. Ben's priority is pulling large `hisRead` datasets from existing Haystack servers into Arrow and Parquet, then querying with DataFusion in either language.

Deliver the snapshot workflow first, then the optional live DataFusion provider. Keep PostgreSQL/TimescaleDB as a separate backend track. Preserve existing ordinary Haystack APIs and authentication behavior.

Work in small, independently testable changes, and continue through the authorized analytics milestones. Do not replace successful validation with a broad rewrite or stop at a scaffold.

This prompt authorizes local analytics implementation and relevant documentation/tests. It does not authorize pushing branches, filing issues, publishing packages, deployments, or migrating external databases. Record backend follow-ups locally unless the user separately asks to implement them.

## Starting context: verify before acting

Assessment baseline: rusty-haystack local `dev` at `40f6e0c4e052215e3e4bededbbf94a075d634df7`; Rust package version 0.9.0, edition 2024, MSRV 1.97. Reference repository: rusty-bacnet at `53bd87d2f0516580d26938a3a3b385e791b756f1`.

The original local paths were `/home/ben/Desktop/rusty-haystack` and `/home/ben/Desktop/rusty-bacnet`; discover equivalent paths if this prompt is used elsewhere. Treat BACnet as a read-only reference.

1. Read applicable AGENTS.md instructions and inspect Git status. Preserve existing user work.
2. Confirm the actual branch/HEAD and refresh this document's findings against current source.
3. Read the following scopes before designing changes:
   - `Cargo.toml`, `Cargo.lock`, all relevant package manifests;
   - `haystack-core/src/kinds/{kind,datetime,tz}.rs`, grid/dict types, Zinc parser/encoder and Codec;
   - `haystack-client/src/{client,config,error}.rs`, transport traits and HTTP implementation;
   - `haystack-server/src/{his_provider,his_store,state,app,error}.rs` and `ops/his.rs`;
   - `rusty-haystack/src/{lib,client,server,data,convert}.rs`, its stubs, packaging and tests;
   - `.github/workflows/{ci,python,release}.yml`;
   - BACnet's `docs/python-api.md`, Python crate packaging, stubs, client lifecycle, `src/py_async/`, and Future/interpreter-exit tests.
4. Capture baseline checks and any environmental failures separately from regressions.
5. Write a compact current-source map and chosen public contracts in a local work note. Proceed on routine decisions using this plan.

Do not report that Python bindings or typing do not exist. PyO3 0.29 and a substantial .pyi already exist in the baseline. No Arrow, Parquet, DataFusion, or SQL implementation was found then.

## Milestone 0: usable Python package and build baseline

Expose the Rust `ClientConfig` functionality to Python with coherent keyword arguments and typed configuration. Include SCRAM/Basic auth, verified TLS defaults, optional explicit lab settings, request timeout, wire format, and existing plaintext-Basic opt-in behavior. New analytics requests must reuse those settings.

Repair existing Python docs examples for actual submodule imports, `WsClient.connect`, and `TlsConfig`. The Python version requirement is currently 3.11+, despite the old 3.8+ sentence.

Explicitly package and verify stubs and appropriate typing markers. Test imports and meaningful type checking against an installed wheel outside the source tree. Source presence is not proof of artifact inclusion. Cover dynamic submodules as well as root exports.

Select a supported Rust toolchain consistent with maintainer intent (1.98.1 or 1.99.0 were suggested), and distinguish it from the declared MSRV. Do not raise MSRV simply to match the build pin. Update only dependencies needed for this work, keeping lockfile changes reviewable.

Separate PyO3's extension-module feature used by maturin from Rust test-linking needs if required, following the proven packaging pattern in BACnet.

## Milestone 1: reliable history semantics and batching

Implement from the current [Haystack history specification](https://project-haystack.org/doc/docHaystack/Ops#hisRead). Follow its request/response formats, half-open ranges, point-timezone behavior, and batch timezone rules. Standard batch requests use grid-meta range and row IDs; value columns map back through column metadata. Do not invent a vendor-only multi-ID shape when the standard shape applies.

Fix the baseline server's row-zero-only handler, date-only parsing, UTC date interpretation, inclusive final-second boundary, missing range metadata, and loss of timezone identity. Validate IDs and reject malformed/mixed request shapes.

Use existing Haystack timezone helpers; enable `chrono-tz` in the appropriate server feature/dependency path if needed. Inject a clock in range-resolution tests instead of relying on today's actual date.

Design a shared history domain contract that retains full `HDateTime` information, values, point/source identity, and resolved range metadata. It must permit explicit read/write errors and bounded ordered streams. Keep Arrow and database types out of this lightweight contract. Extract a small crate only if it improves dependency direction; avoid forcing an analytics consumer to depend on the server just to name a history sample.

Preserve the in-memory store's documented replace-on-duplicate semantics. Make its retention policy visible and distinguish a retained dataset from a complete historical archive. Do not silently reuse its million-sample cap as an analytics export limit.

Provide request modes `single`, `batch`, and `auto`. Handle older remote servers using a narrow capability probe or configured capability. Cache it per source/session. Do not interpret auth, timeout, parser, or generic server failures as permission to silently change request mode.

## Milestone 2: shared history → Arrow implementation

Add an optional Rust analytics crate/module with one authoritative conversion implementation shared by Python, Parquet, and DataFusion.

Use the human plan's long-table v1 schema:
`source, point_id, ts, tz, offset_seconds, value_kind, value_num, value_bool, value_str, unit, value_zinc`.

Contract requirements:

- Required identity and timestamp columns; stable nullability and schema metadata.
- `ts` is UTC nanoseconds with checked range/precision conversion.
- Retain returned timezone name and UTC offset separately.
- `unit` is the raw sample-number unit; do not silently substitute point metadata or convert units.
- Typed numeric/bool/string projections, plus canonical Zinc value serialization for reconstruction.
- Preserve NA versus Null, signed infinities/NaN, unit-bearing numbers, Refs/display names, Marker/Remove, and supported complex values.
- No Debug-string serialization, lossy blanket stringification, or automatic Float64 conversion of all values.
- A batch alignment gap is not a sample. Qualify the upstream Null ambiguity described in the plan; allow single-point retrieval when faithful distinction is required.
- Schema stays fixed across empty/large/mixed-kind batches and multiple points.
- Row-size, batch-row, and batch-byte limits are enforced with typed errors.
- Point metadata export is a separate keyed snapshot, retaining a canonical entity representation.

Do not make generic lossless Arrow conversion of every arbitrary grid a prerequisite for history analytics. If an `HGrid.to_arrow` convenience is added, specify its narrower contract and error on unsupported shapes.

Choose mutually compatible Arrow/PyO3/DataFusion versions. The researched starting family was PyO3 0.29, pyo3-arrow 0.19, Arrow/Parquet 59, DataFusion/FFI 55; verify current compatibility rather than copying the newest unrelated releases. Use workspace version declarations to avoid duplicate Arrow majors.

## Milestone 3: bounded remote ingestion and Parquet exports

Add a history source that yields Arrow batches without building a complete response String/HGrid first.

Primary path: authenticated HTTP Zinc, decoded incrementally using existing lexical/value rules. Add the reqwest streaming feature or equivalent supported body-chunk API as required. Decode request/response Content-Type correctly; retain error-grid handling.

Handle headers, column metadata, partial UTF-8, quoted escapes, nested values, row boundaries, truncated transport bodies, and parse errors. Do not implement a naive newline splitter. Existing Zinc header/row encoders are reusable for server responses.

Bound queued work by bytes and batches, not only rows. Propagate consumer backpressure to decode/fetch. Bound maximum token/value size and point count per standard batch request. Do not allow an upstream `Vec` or background task to accumulate the entire result behind a stream-shaped facade.

Single-request mode should remain available. Windowed fallback should use explicit point/time envelopes, bounded concurrency, deterministic ordering per series, and strict boundary deduplication. Specify read retries separately from writes; no unlimited retries or guessed continuation tokens.

Inspect [incomplete response metadata](https://project-haystack.org/doc/docHaystack/HttpApi#incompleteData). Fail or use a configured smaller-window recovery; never mark an incomplete response complete. If a minimum window still cannot be qualified, return an actionable completeness error.

Expose a Rust Arrow batch stream and a synchronous Python reader with:
`__arrow_c_stream__`, close/context management, and a documented one-consumer policy. Capsule calls must create correctly owned one-use stream exports or reject repeat use predictably. Release producer tasks/resources on close, exhaustion, consumer errors, and early abandonment. No private Rust ABI handoff between independent extensions.

Implement the Arrow boundary using [the PyCapsule protocol](https://arrow.apache.org/docs/format/CDataInterface/PyCapsuleInterface.html), preferably via a compatible maintained bridge. Detach Python around blocking pulls and native work; never hold borrowed Python references or the PyO3 attachment while blocking on callbacks that need Python.

Implement the Parquet writer in Rust and bind it to Python:

- One-file export first; schema must match the Arrow contract.
- Configurable compression, record-batch ceilings, row-group ceilings, and writer-memory thresholds.
- Finalize footer successfully, validate summary, then publish a staging file with explicit overwrite behavior.
- Refuse overwriting existing user data by default.
- Write a manifest and metadata snapshot with source identity, schema/codec version, requested/resolved range, point IDs, counts, completion state, and file integrity evidence.
- Redact credentials, bearer tokens, and sensitive URL parameters from metadata and errors.
- No implicit append to a Parquet file; no duplicate visible snapshots in a recursively scanned dataset.
- Add partitioned snapshots/resume only after the basic export passes. Use UTC dates and a defined snapshot identity; prevent tiny-file proliferation and define correction handling.

For a first server-side streaming implementation, support the qualified Zinc path and return explicit limits/errors for buffered formats. Do not claim every codec has bounded streaming. Mid-stream errors must invalidate completion and prevent successful artifact publication.

Demonstrate the same exported file in Rust DataFusion and Python DataFusion. SQL examples must keep source identity and units in aggregates, or explicitly convert units.

## Milestone 4: async Python ergonomics and interpreter support

Keep `rusty_haystack.client.HaystackClient` usable for existing synchronous workflows. Add `AsyncHaystackClient` with typed awaitable I/O and `async with`, consistent with BACnet's visible conventions. Decide and document whether native methods return Futures or coroutines; stubs must reflect reality.

Add async history acquisition/iteration and cancellation. Arrow's C stream pull interface is synchronous; do not market its `get_next` as a coroutine. Provide a clear synchronous Arrow reader path and an async API where supported, sharing the Rust source.

Choose a compatible maintained Tokio/asyncio bridge when possible. If using BACnet's custom bridge as a reference, include cancellation, event-loop closure, panic conversion, native unit→Python None, and interpreter-finalization behavior. Do not copy a fragment and claim parity.

No blocking sync `block_on` inside a Python event-loop call or DataFusion Tokio worker. Use owned `Send` data, bounded channels, and a runtime/lifetime design with tested shutdown.

Audit shared/mutable classes, callbacks, lock order, runtime initialization, and stream destruction. [PyO3 0.29](https://pyo3.rs/v0.29.0/free-threading.html) already defaults modules to free-threading support; macro decoration alone is not qualification.

Build/test ordinary CPython 3.14 and free-threaded 3.14t wheels separately. Retain 3.11+ unless current project policy chooses otherwise. Check actual optional PyArrow/DataFusion artifact support under each interpreter; report unsupported combinations accurately.

Update existing workflows only as needed for implemented support. Distinguish a test interpreter matrix from a published wheel matrix. Verify installed artifacts rather than import from the repository by accident.

## Milestone 5: optional live DataFusion adapter

Implement a read-only Rust `HaystackHistoryTable` provider in a separate analytics adapter crate and optional Python plugin.

Direct-query contract:

- Constructor receives ordinary URL/auth/TLS configuration, source identity, explicit IDs, and a finite start/end envelope.
- Schema is fixed and registration does not download history.
- Planning creates an execution plan; remote reads occur during execution.
- Execution yields the shared Arrow schema with backpressure and cancellation.
- Push down point-ID equality/IN and safe timestamp bounds by intersecting with the configured envelope.
- Preserve unsupported/inexact predicates for engine evaluation; do not report Exact unless source evaluation is proven equivalent.
- Projection avoids constructing unrequested columns while retaining fields necessary for residual predicates.
- No unsupported aggregate pushdown, remote SQL eval, or automatic string rewriting into Haystack filters.
- Do not push SQL LIMIT ahead of a residual filter or claim global ordering from per-series order.
- Bound concurrency and expose useful fetch/byte/batch metrics.
- A canceled scan drops pending requests and eventually releases session/client resources.

Use the selected DataFusion version's actual `TableProvider`, `ExecutionPlan`, and stream APIs. Consult [the official provider guide](https://datafusion.apache.org/library-user-guide/custom-table-providers.html). Old copied signatures are not an implementation.

Export the Python provider using [`__datafusion_table_provider__`](https://datafusion.apache.org/python/user-guide/io/table_provider.html) and qualified `datafusion-ffi` capsule/version checks. Its documentation examples can lag current Rust constructors: compile against the chosen versions.

Test direct registration with the user's separately installed `datafusion.SessionContext`. FFI compatibility and ownership must be proven; do not transmute a Rust trait object between wheels. A new plugin client can receive the same configuration data as the base Python package.

Provide two documented paths:
1. In-memory `ctx.from_arrow(reader)` for bounded data; measure its import behavior.
2. Lazy `ctx.register_table("history", provider)` for live remote reads.

Neither Arrow C-stream export nor `from_arrow` proves lazy SQL fetches. Do not implement the live provider using a hidden `collect()`/MemTable that downloads everything during registration.

## Optional storage follow-up: define, do not fold into the analytics deliverable

After shared history semantics stabilize, record a concrete S1 plan for a Postgres adapter. Do not implement it in this run unless the user has requested that track.

The adapter needs fallible streamed reads, transactional writes, duplicate/correction policy, migrations, restart durability, pooling, source/point isolation, retention settings, and exact kind/unit/time handling. Preserve nanoseconds/timezone identity separately from Postgres's datetime convenience column. TimescaleDB is optional and needs its own qualified layout.

Entity graph persistence is a separate contract; existing `HistoryProvider` does not wrap `SharedGraph`. A Parquet-backed writable store additionally needs manifests, compaction, correction visibility, and recovery. SeleneDB needs actual API evidence before an adapter is designed.

## Acceptance tests: meaningful evidence

Use deterministic local/mock servers and temporary artifacts. Real Niagara/SkySpark checks can supplement local tests if supplied, but must not be invented.

History and decode:
- Exact start included, exact end excluded, fractional final-second sample retained.
- DST skipped/repeated local times and date windows; invalid/reversed/unknown-timezone ranges.
- Standard single/batch wire fixtures, column-ID mapping, mixed-zone requests, uneven sample timestamps.
- Batch unsupported fallback distinguished from auth/timeouts; incomplete response and truncated-body rejection.
- UTF-8, strings, nested scalars, metadata, and rows split across arbitrary byte boundaries.
- Compare incremental results with the existing full decoder on deterministic accepted fixtures.

Arrow/Parquet:
- Numeric/bool/string/NA/Null/Ref/unit/timezone fidelity; non-finite floats; checked timestamp range.
- Fixed schema across chunks and empty results; canonical-value reconstruction.
- Multi-source identities, variable sample units, metadata snapshot joins.
- Independent Arrow/Parquet reader and SQL result comparison, not just writer self-comparison.
- Disk errors, cancellation, and malformed input never publish a successful output.
- Existing destination remains intact when overwrite is refused.
- Repeated snapshot/resume policy does not duplicate or lose observations.

Python/package/lifecycle:
- Installed-wheel typing/imports without source-path assistance.
- Arrow C-stream import into PyArrow and DataFusion on qualified versions.
- Buffer lifetimes after producer Python objects are dropped; predictable repeated reader use.
- Async event-loop progress during native fetch; timeout/cancel/close behavior.
- Ordinary and free-threaded interpreter import, concurrent calls, iterator mutation policy, and subprocess-exit tests.
- Optional packages absent: base imports and existing clients still work.

Live DataFusion:
- Registration and EXPLAIN issue zero history downloads.
- Actual filtered queries fetch fewer IDs/windows than an unfiltered scan.
- Query results equal a trusted local fixture for supported and residual filters.
- LIMIT with residual filter; empty/false predicates; repeated execution semantics.
- Projection schema, cancellation, FFI version mismatch, and finite-envelope bounds.
- Validate Rust and Python provider surfaces against the same fixture data.

Performance and dependency isolation:
- Generate at least 1M deterministic history samples without preloading them all in the producer.
- Capture time to first batch, total time, peak RSS, request/byte counts, max queued bytes/batches, and writer memory.
- Compare 100K versus 1M scans at identical limits; explain scaling and bounded queues. Do not promise a particular speedup before measurement.
- Distinguish remote server buffering from client-side memory guarantees.
- Verify core/client/server dependency trees do not include DataFusion or Postgres; verify the Parquet path does not require DataFusion.

## Working and completion rules

Run existing formatting/lint/test checks appropriate to touched packages and features. Baseline examples include:

```sh
cargo fmt --all --check
cargo test --workspace --exclude rusty-haystack
cargo test -p rusty-haystack-core --features chrono-tz
cargo clippy --workspace --exclude rusty-haystack --all-targets -- -D warnings
```

New adapters may make `--workspace` intentionally heavy; provide explicit default-package and adapter checks. Do not omit analytics crates from qualification merely to retain old command speed.

Build native Python extensions in an isolated venv using the repository's existing toolchain/uv/maturin conventions. Run the existing pytest suite plus new qualified feature tests. Test the built wheel in a fresh environment. Use the selected interpreter explicitly for each wheel profile.

For each milestone, update public docs/stubs/examples affected by the change and record actual commands/results. Keep future APIs labeled future until working. Include fixtures and measured evidence for correctness/performance claims.

Finish with:
- implemented user workflow and exact examples;
- files/packages changed and intentional API changes;
- validation and benchmark evidence with versions;
- remaining limitations and local issue drafts;
- separate backend follow-up recommendations.

Do not publish wheels, push, or file external issues as an implicit completion step.

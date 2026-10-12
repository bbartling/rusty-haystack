# PostgreSQL application authority

The optional `rusty-haystack-app/postgres` feature selects one bounded PostgreSQL
18.6 dataset for the existing authorized entity and history services. Default
builds continue to use explicit process-memory stores and do not enable SQLx.
This first profile supports one application owner for each database/schema. It
is not multi-instance cache coherence, a migration system, replication or backup
qualification, a performance claim, or a production deployment recipe.

## Select and own the application

Call `haystack_app::postgres::application(config, catalog, policies, limits).await`
to recover a builder. `PostgresPolicies` separately supplies read, entity-write
and history-write authorization. Its selected services share the same managed
read admission and lifecycle. Add listeners/resources to the returned builder,
start it on the borrowed Tokio runtime, and await `owner.ready()` before use.
Database validation/recovery finishes before the builder is returned; readiness
also verifies its ownership connection before any selected listener serves.

Configuration uses explicit host, port, database, username, optional password,
schema and transport fields. There is no connection-URL or passfile constructor.
The environment must remain immutable during configuration. Presence of any of
`PGPORT`, `PGHOSTADDR`, `PGHOST`, `PGUSER`, `PGDATABASE`, `PGPASSWORD`,
`PGSSLROOTCERT`, `PGSSLCERT`, `PGSSLKEY`, `PGSSLMODE`, `PGAPPNAME` or `PGOPTIONS`
is rejected, including an empty or non-Unicode value. Public errors and Debug
omit driver diagnostics, endpoints, credentials and certificate material.

`PostgresTransport::VerifyFull` requires certificate-chain and hostname
verification, using bundled WebPKI roots plus an optional explicit CA. The
additional CA is not an exclusive trust pin. There is no plaintext fallback.
`LoopbackPlaintext` is a separate explicit development/test selection limited to
`localhost` and literal loopback addresses. Schema names pass a narrow ASCII
identifier allowlist and cannot be `public` or start with `pg_`.

The application requires PostgreSQL 18 or later, logged authority tables and
`fsync`, `synchronous_commit` and `full_page_writes` enabled. Runtime qualification
currently pins 18.6; the version check does not qualify every later release.
Every connection fixes statement, lock, transaction and idle-transaction limits,
`synchronous_commit=on` and the selected search path. Startup compares the stored
catalog selection with the supplied activated catalog. A mismatched catalog,
unsupported format, corrupt projection or exceeded recovery bound fails before
readiness. It never truncates stored data to make startup succeed.

The constructor owns a bounded background construction task. Dropping its future
cannot turn an unfinished driver operation into a cleanup receipt; failed
builder delivery awaits backend closure. Dropping an unstarted recovered builder
requests database cleanup. Keep the borrowed runtime driving. For a started
application, await `owner.close()` and inspect `owner.terminated()` for actual
work completion and cleanup errors, as in the [lifecycle contract](application-lifecycle.md).

## Authority and atomic publication

One dedicated, unpooled session holds the schema's advisory ownership lock. It
is never transparently reconnected. New managed admissions verify that session
and its durable owner generation under their bounded read permit. Every write
transaction locks the dataset metadata row and verifies the generation again.
Lock/session loss or an unresolved publication failure seals the backend; a new
owner may recover it, while stale services cannot resume reads or writes.

Only the application owns mutable projections. `ReadService::graph()` and
`EntityMutationStore::graph()` expose `GraphView`, a read-only inspection handle.
`native_graph()` returns `None` for this profile. Legacy native entity writes,
native history write/reset, simulated commands and hot catalog replacement are
unavailable rather than bypassing SQL authority. Ephemeral selections retain an
explicit native capability. Graph inspection is trusted local observation, not
a substitute for managed read authorization or a live ownership probe.

Entity commits serialize their full bounded graph/change projection, stable
identity, revision/floor and receipt in one SQL transaction. History commits
serialize the affected point's samples, generation, history sequence/change and
receipt in one SQL transaction. Candidate graph indexes, sample vectors and
encoded payloads are prepared before COMMIT. Public projections and notifications
advance only after the durable receipt proves the commit. A rolled-back write
consumes no public entity revision or history sequence.

Operation IDs bind the stable principal kind/subject and exact original
canonical typed request. Retries with the same intent return the stored receipt;
changed intent conflicts. This includes original history ordering, duplicate
samples and DateTime offsets, even when normalized sample effects are equal.
A pending identity is durable and cannot be reused to replay an uncertain write.
An ambiguous COMMIT is reconciled only after a fresh metadata-row serialization
barrier and subsequent READ COMMITTED receipt query. A plain absence lookup is
not no-effect proof. Failure to complete the barrier remains `Unknown` and seals
the owner. SQL commit followed by lost in-memory publication is recovered on
restart; it is never reported as `Rejected`.

Committed receipts use the explicit `PostgresDurable` / `postgresDurable`
qualification. That label refers to this selected store contract and required
PostgreSQL settings. It does not certify underlying hardware, operating-system
flush behavior, external storage configuration, backups or disaster recovery.

Canonical typed payloads remain authoritative. SQL IDs, unsigned revisions via
`numeric(20,0)`, and sample `(seconds,nanoseconds)` keys are checked projections.
Payloads preserve rich kinds, explicit null versus absence, exact integer and
float bits, units, nominal catalog/revision provenance and original temporal
representation. Recovery rebuilds indexes from bounded primary rows and validates
complete retained change units. Entity feed identity and signing key survive
restart; feed token expiry uses wall-clock time. Normal page cursors and active
snapshot sessions do not survive application restart.

## Bounds, reads and cleanup

`PostgresLimits` bounds pool capacity, acquisition/statement/lock/transaction and
snapshot duration, primary/change recovery rows/bytes/work, receipt count/bytes
and total history samples. Existing service-specific admission, work, retained
memory and history retention limits still apply. Oversized writes fail before
effects; recovery refuses oversized persisted state. Receipts are retained within
a finite capacity; this profile does not silently evict identity bindings.

Ordinary entity reads use the coherent application projection. Ordinary history
continues to advertise `HistoryCapabilities::BOUNDED_LIVE`; persistence does not
upgrade it to snapshot history or imply a cross-store read transaction.

For a fixed entity view, call `reads.postgres_snapshot(context).await` or transfer
an existing `ReadAdmission` into `postgres_snapshot()`. The returned
`PostgresSnapshot` owns one `REPEATABLE READ READ ONLY` transaction and bounded
recovered graph until close. `snapshot.read(request).await` uses the existing
filter, traversal, reference, policy, encoding and cursor engine against that
same view. Policy is refreshed on every page. Budgets accumulate across pages
under the original deadline and admission slot; callers cannot create a fresh
unbounded service from the snapshot. It covers entities only.

Explicitly await `snapshot.close()` for transaction, physical connection and work
registration cleanup. Dropping it or abandoning a page requests cancellation;
the worker retains its admission and pool capacity until cleanup finishes.
Owner shutdown observes that actual work. Transactions deliberately retire their
physical connection after acknowledged COMMIT/ROLLBACK in this first profile.
This prevents SQLx's queued Drop rollback/pool-return path from standing in for
completed cleanup. It has a connection setup cost and makes no throughput claim.
SQLx pools remain bounded; reusable build caches are unrelated to these resources.

## Explicit qualification

The baseline local gate remains unchanged. Run it and the selected PostgreSQL
profile separately. The profile requires Docker, OpenSSL, Rust 1.99.0 and
cargo-nextest 0.9.146:

```sh
python3 scripts/postgres/qualify.py --docker
```

The script creates two uniquely named loopback containers, an explicitly owned
PostgreSQL 18 volume and disposable TLS material using the pinned 18.6 image
index digest. It bounds container resources, verifies server settings, supplies
private fixture files, and removes only its own resources in `finally`. Existing
fixtures can be used without transferring their lifecycle:

```sh
python3 scripts/postgres/qualify.py \
  --fixture /absolute/private/tls.json \
  --plain-fixture /absolute/private/plain.json
```

Fixture JSON contains `host`, `port`, `database`, `user`, `password`,
`tls_server_name` and `root_certificate`. Keep it private; it is not a product
configuration or connection URL. The explicit test selection fails on missing
fixtures or an empty test inventory. Ignored runtime tests in a default build are
not qualification evidence.

The required Ubuntu `PostgreSQL 18.6 durability` CI job runs optional-feature
Clippy, app tests/doctests, deterministic fixture-startup checks, and the real
profile. The aggregate `CI OK` requires
that job to succeed. Tests cover restart and typed data, durable receipt binding,
real session termination before commit, discarded commit acknowledgment,
commit-before-publication recovery, unresolved barrier/lease retention,
independent-process ownership exclusion, live-socket generation fencing,
history retention/watermark restart, reservation-hit reconciliation authorization,
verified TLS and negative endpoints, bounded/corrupt recovery, and
snapshot/reference coherence and cleanup. A one-connection pool case verifies
cancelled and expired snapshot waiters, actual lease/transaction cleanup, and
fresh capacity after releasing the held snapshot.
Discarded acknowledgment and held-commit scheduling use explicit
client fault seams over real SQL transactions; they are not a network-partition
or power-loss test. See the [support matrix](support-matrix.md) for host and
feature limits.

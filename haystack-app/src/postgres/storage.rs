use super::{PostgresConfig, PostgresError, PostgresLimits};
use sqlx::{Connection, PgConnection, PgPool, Postgres, Row, postgres::PgPoolOptions};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

/// Shared database authority. Every supported transaction participates in the
/// schema gate before acquiring its metadata row lock. Epochs fence restores.
pub(crate) struct Backend {
    pub(crate) pool: PgPool,
    pub(crate) schema: String,
    pub(crate) projection: std::sync::OnceLock<super::refresh::Projection>,
    pub(crate) generation: i64,
    pub(crate) limits: PostgresLimits,
    pub(crate) runtime: tokio::runtime::Handle,
    pub(crate) writer: parking_lot::Mutex<()>,
    sealed: AtomicBool,
    #[cfg(test)]
    pub(crate) fault: parking_lot::Mutex<Option<CommitFault>>,
    #[cfg(test)]
    pub(crate) refresh_fault: parking_lot::Mutex<Option<RefreshFault>>,
    #[cfg(test)]
    pub(crate) provisional_revision: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    pub(crate) reservation_seed: parking_lot::Mutex<Option<StoredReceipt>>,
    #[cfg(test)]
    pub(crate) snapshot_acquiring: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(crate) snapshot_setup_close: parking_lot::Mutex<Option<SetupCloseFault>>,
}

pub(crate) struct Opened {
    pub(crate) connection: PgConnection,
    pub(crate) config: PostgresConfig,
    pub(crate) options: sqlx::postgres::PgConnectOptions,
}
impl Opened {
    pub(crate) async fn connect(config: PostgresConfig) -> Result<Self, PostgresError> {
        let options = config.options()?;
        let mut connection = tokio::time::timeout(
            config.limits.acquire_timeout,
            PgConnection::connect_with(&options),
        )
        .await
        .map_err(|_| PostgresError::Deadline)?
        .map_err(|_| PostgresError::Connection)?;
        let initialized=tokio::time::timeout(config.limits.transaction_timeout,async {
        let safe = sqlx::query("SELECT current_setting('server_version_num')::int AS version, current_setting('fsync') AS fsync, current_setting('synchronous_commit') AS sync, current_setting('full_page_writes') AS full")
            .fetch_one(&mut connection).await.map_err(|_| PostgresError::Connection)?;
        if safe
            .try_get::<i32, _>("version")
            .map_err(|_| PostgresError::Durability)?
            < 180000
            || safe
                .try_get::<String, _>("fsync")
                .map_err(|_| PostgresError::Durability)?
                != "on"
            || safe
                .try_get::<String, _>("sync")
                .map_err(|_| PostgresError::Durability)?
                != "on"
            || safe
                .try_get::<String, _>("full")
                .map_err(|_| PostgresError::Durability)?
                != "on"
        {
            return Err(PostgresError::Durability);
        }
        sqlx::query(
            "SELECT pg_advisory_lock(hashtextextended(current_database() || ':' || $1, 0))",
        )
        .bind(&config.schema)
        .execute(&mut connection)
        .await
        .map_err(|_| PostgresError::Connection)?;
        sqlx::query("BEGIN ISOLATION LEVEL READ COMMITTED").execute(&mut connection).await.map_err(|_| PostgresError::Connection)?;
        // The identifier has passed the ASCII allowlist before interpolation.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE SCHEMA IF NOT EXISTS \"{}\"",
            config.schema
        )))
        .execute(&mut connection)
        .await
        .map_err(|_| PostgresError::Incompatible)?;
        sqlx::raw_sql(SCHEMA)
            .execute(&mut connection)
            .await
            .map_err(|_| PostgresError::Incompatible)?;
        // This is the sole supported transition. Holding the original
        // exclusive key prevents coexistence with a live format-1 owner.
        let format: Option<i32> = sqlx::query_scalar("SELECT format FROM metadata WHERE singleton=1")
            .fetch_optional(&mut connection).await.map_err(|_|PostgresError::Incompatible)?;
        match format {
            Some(1) => { sqlx::raw_sql("ALTER TABLE metadata DROP CONSTRAINT metadata_format_check; ALTER TABLE metadata ADD CONSTRAINT metadata_format_check CHECK(format=2) NOT VALID; UPDATE metadata SET format=2; ALTER TABLE metadata VALIDATE CONSTRAINT metadata_format_check;").execute(&mut connection).await.map_err(|_|PostgresError::Incompatible)?; }
            Some(2) | None => {},
            _ => return Err(PostgresError::Incompatible),
        }
        // A pre-existing unlogged table does not become logged through IF NOT
        // EXISTS. Check every authority table, not merely our metadata row.
        let tables = sqlx::query("SELECT relname, relpersistence::text AS persistence FROM pg_class JOIN pg_namespace ON pg_namespace.oid = relnamespace WHERE nspname = $1 AND relname = ANY($2)")
            .bind(&config.schema).bind(TABLES.as_slice()).fetch_all(&mut connection).await
            .map_err(|_| PostgresError::Incompatible)?;
        if tables.len() != TABLES.len()
            || tables
                .iter()
                .any(|row| row.try_get::<String, _>("persistence").ok().as_deref() != Some("p"))
        {
            return Err(PostgresError::Durability);
        }
        validate_schema(&mut connection, &config.schema).await?;
        sqlx::query("COMMIT").execute(&mut connection).await.map_err(|_|PostgresError::Connection)?;
        Ok(())
        }).await.unwrap_or(Err(PostgresError::Deadline));
        if let Err(error) = initialized {
            let _ = tokio::time::timeout(config.limits.statement_timeout, connection.close()).await;
            return Err(error);
        }
        Ok(Self {
            connection,
            config,
            options,
        })
    }
    pub(crate) async fn finish(self, generation: i64) -> Result<Arc<Backend>, PostgresError> {
        // Physically closing the initialization session releases its exclusive
        // gate before the first shared recovery transaction or admission.
        tokio::time::timeout(
            self.config.limits.statement_timeout,
            self.connection.close(),
        )
        .await
        .map_err(|_| PostgresError::Deadline)?
        .map_err(|_| PostgresError::Connection)?;
        let pool = PgPoolOptions::new()
            .max_connections(self.config.limits.pool_connections)
            .min_connections(0)
            .acquire_timeout(self.config.limits.acquire_timeout)
            .test_before_acquire(true)
            .connect_lazy_with(self.options);
        Ok(Arc::new(Backend {
            pool,
            schema: self.config.schema,
            projection: std::sync::OnceLock::new(),
            generation,
            limits: self.config.limits,
            runtime: tokio::runtime::Handle::current(),
            writer: parking_lot::Mutex::new(()),
            sealed: AtomicBool::new(false),
            #[cfg(test)]
            fault: parking_lot::Mutex::new(None),
            #[cfg(test)]
            refresh_fault: parking_lot::Mutex::new(None),
            #[cfg(test)]
            provisional_revision: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            reservation_seed: parking_lot::Mutex::new(None),
            #[cfg(test)]
            snapshot_acquiring: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            snapshot_setup_close: parking_lot::Mutex::new(None),
        }))
    }
}
impl Backend {
    pub(crate) fn seal(&self) {
        self.sealed.store(true, Ordering::Release);
    }
    pub(crate) fn check(&self) -> Result<(), PostgresError> {
        if self.sealed.load(Ordering::Acquire) {
            Err(PostgresError::Unavailable)
        } else {
            Ok(())
        }
    }
    pub(crate) fn publication_guard(
        &self,
        deadline: Instant,
    ) -> Result<parking_lot::MutexGuard<'_, ()>, PostgresError> {
        loop {
            self.check()?;
            if Instant::now() >= deadline {
                return Err(PostgresError::Deadline);
            };
            if let Some(guard) = self
                .writer
                .try_lock_for(std::time::Duration::from_millis(5))
            {
                self.check()?;
                return Ok(guard);
            }
        }
    }
    pub(crate) async fn health(self: &Arc<Self>) -> Result<(), PostgresError> {
        self.refresh_until(Instant::now() + self.limits.transaction_timeout)
            .await
    }
    pub(crate) async fn refresh_until(
        self: &Arc<Self>,
        deadline: Instant,
    ) -> Result<(), PostgresError> {
        let backend = self.clone();
        self.runtime
            .spawn_blocking(move || backend.refresh(deadline))
            .await
            .map_err(|_| PostgresError::Unavailable)?
    }
    pub(crate) async fn schema_shared(
        &self,
        connection: &mut PgConnection,
    ) -> Result<(), PostgresError> {
        sqlx::query(
            "SELECT pg_advisory_lock_shared(hashtextextended(current_database() || ':' || $1, 0))",
        )
        .bind(&self.schema)
        .execute(connection)
        .await
        .map_err(|_| PostgresError::Unavailable)?;
        Ok(())
    }
    pub(crate) async fn transaction(
        &self,
        deadline: Instant,
    ) -> Result<OwnedTransaction, PostgresError> {
        self.locked_transaction(deadline, true).await
    }
    pub(crate) async fn read_transaction(
        &self,
        deadline: Instant,
    ) -> Result<OwnedTransaction, PostgresError> {
        self.locked_transaction(deadline, false).await
    }
    async fn locked_transaction(
        &self,
        deadline: Instant,
        write: bool,
    ) -> Result<OwnedTransaction, PostgresError> {
        self.check()?;
        #[cfg(test)]
        let acquiring = Acquisition::new(&self.snapshot_acquiring);
        let transaction = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.pool.acquire(),
        )
        .await
        .map_err(|_| PostgresError::Deadline)?
        .map_err(|_| PostgresError::Unavailable)?;
        #[cfg(test)]
        drop(acquiring);
        let mut transaction =
            OwnedTransaction::begin(transaction, false, self.limits.statement_timeout).await?;
        let result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
            self.schema_shared(&mut transaction).await?;
            let statement = if write {
                "SELECT owner_generation,format FROM metadata WHERE singleton=1 FOR UPDATE"
            } else {
                "SELECT owner_generation,format FROM metadata WHERE singleton=1 FOR SHARE"
            };
            let row = sqlx::query(statement)
                .fetch_one(&mut *transaction)
                .await
                .map_err(|_| PostgresError::Unavailable)?;
            if row
                .try_get::<i64, _>("owner_generation")
                .map_err(|_| PostgresError::Incompatible)?
                != self.generation
                || row
                    .try_get::<i32, _>("format")
                    .map_err(|_| PostgresError::Incompatible)?
                    != 2
            {
                self.seal();
                return Err(PostgresError::Incompatible);
            }
            if let Some(projection) = self.projection.get() {
                // Held preparations may reach this barrier without another
                // admission. Validate immutable authority before any effect,
                // even when an unsupported administrator change left the epoch
                // unchanged. Copy local identity before the SQL wait.
                let state = projection.graph.state();
                let history = projection.history.upgrade().ok_or(PostgresError::Unavailable)?;
                let compatible: bool = sqlx::query_scalar("SELECT dataset=$1 AND incarnation=$2 AND catalog=$3 AND catalog_generation::text=$4 AND history_authority=$5 AND cursor_key=$6 FROM metadata WHERE singleton=1")
                    .bind(projection.dataset.as_slice())
                    .bind(state.incarnation.as_slice())
                    .bind(projection.catalog.selection_identity())
                    .bind(state.catalog_generation.to_string())
                    .bind(history.authority.as_slice())
                    .bind(projection.cursor_key.as_slice())
                    .fetch_one(&mut *transaction).await.map_err(read_error)?;
                if !compatible {
                    self.seal();
                    return Err(PostgresError::Incompatible);
                }
            }
            Ok(())
        })
        .await
        .unwrap_or(Err(PostgresError::Deadline));
        match result {
            Ok(()) => Ok(transaction),
            Err(error) => {
                let _ = transaction.rollback().await;
                Err(error)
            }
        }
    }
    pub(crate) async fn close_snapshot_setup(
        &self,
        connection: sqlx::pool::PoolConnection<Postgres>,
    ) {
        #[cfg(test)]
        {
            let fault = self.snapshot_setup_close.lock().take();
            if let Some(fault) = fault {
                let _ = fault.entered.send(());
                let _ = tokio::time::timeout(self.limits.transaction_timeout, fault.release).await;
            }
        }
        // Retain the original worker, work lease and pool permit through
        // bounded physical retirement. Drop alone spawns driver cleanup.
        let _ = tokio::time::timeout(self.limits.statement_timeout, connection.close()).await;
    }
    pub(crate) async fn close(&self) -> Result<(), PostgresError> {
        self.seal();
        // Resource close executes only after admitted workers and sessions have
        // released their transactions; SQLx close waits for actual pool release.
        self.pool.close().await;
        Ok(())
    }
}

const TABLES: [&str; 7] = [
    "metadata",
    "entities",
    "entity_changes",
    "receipts",
    "history_points",
    "history_samples",
    "history_changes",
];
// Format 2 is an exact supported relation contract, not merely IF NOT EXISTS.
const PRIMARY_KEYS: &[(&str, &[&str])] = &[
    ("metadata", &["singleton"]),
    ("entities", &["id"]),
    ("entity_changes", &["revision"]),
    ("receipts", &["domain", "binding"]),
    ("history_points", &["id"]),
    ("history_samples", &["point", "seconds", "nanos"]),
    ("history_changes", &["sequence"]),
];
async fn validate_schema(connection: &mut PgConnection, schema: &str) -> Result<(), PostgresError> {
    let keys = sqlx::query("SELECT c.relname,array_agg(a.attname::text ORDER BY k.ordinality) AS columns FROM pg_constraint p JOIN pg_class c ON c.oid=p.conrelid JOIN pg_namespace n ON n.oid=c.relnamespace CROSS JOIN LATERAL unnest(p.conkey) WITH ORDINALITY AS k(attnum,ordinality) JOIN pg_attribute a ON a.attrelid=c.oid AND a.attnum=k.attnum WHERE p.contype='p' AND n.nspname=$1 AND c.relname=ANY($2) GROUP BY c.relname")
        .bind(schema).bind(TABLES.as_slice()).fetch_all(&mut *connection).await.map_err(read_error)?;
    if keys.len() != PRIMARY_KEYS.len()
        || PRIMARY_KEYS.iter().any(|(table, columns)| {
            !keys.iter().any(|row| {
                row.try_get::<String, _>("relname").ok().as_deref() == Some(*table)
                    && row
                        .try_get::<Vec<String>, _>("columns")
                        .is_ok_and(|actual| {
                            actual
                                .iter()
                                .map(String::as_str)
                                .eq(columns.iter().copied())
                        })
            })
        })
    {
        return Err(PostgresError::Incompatible);
    }
    let format: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_constraint p JOIN pg_class c ON c.oid=p.conrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 AND c.relname='metadata' AND p.conname='metadata_format_check' AND p.contype='c' AND p.convalidated AND pg_get_constraintdef(p.oid)='CHECK ((format = 2))')")
        .bind(schema).fetch_one(&mut *connection).await.map_err(read_error)?;
    let history_reference: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_constraint p JOIN pg_class c ON c.oid=p.conrelid JOIN pg_namespace n ON n.oid=c.relnamespace JOIN pg_class target ON target.oid=p.confrelid WHERE n.nspname=$1 AND c.relname='history_samples' AND target.relnamespace=c.relnamespace AND target.relname='history_points' AND p.contype='f' AND p.convalidated AND pg_get_constraintdef(p.oid)='FOREIGN KEY (point) REFERENCES history_points(id)')")
        .bind(schema).fetch_one(&mut *connection).await.map_err(read_error)?;
    if !format || !history_reference {
        return Err(PostgresError::Incompatible);
    }
    Ok(())
}
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS metadata (
 singleton smallint PRIMARY KEY CHECK(singleton = 1),
 format integer NOT NULL CHECK(format = 2),
 dataset bytea NOT NULL CHECK(octet_length(dataset)=16),
 incarnation bytea NOT NULL CHECK(octet_length(incarnation)=16),
 revision numeric(20,0) NOT NULL CHECK(revision >= 0),
 catalog_generation numeric(20,0) NOT NULL CHECK(catalog_generation > 0),
 floor numeric(20,0) NOT NULL CHECK(floor >= 0 AND floor <= revision),
 history_authority bytea NOT NULL CHECK(octet_length(history_authority)=16),
 history_sequence numeric(20,0) NOT NULL CHECK(history_sequence >= 0),
 owner_generation bigint NOT NULL CHECK(owner_generation > 0),
 catalog text NOT NULL, cursor_key bytea NOT NULL CHECK(octet_length(cursor_key)=32)
);
CREATE TABLE IF NOT EXISTS entities (id text PRIMARY KEY, payload bytea NOT NULL);
CREATE TABLE IF NOT EXISTS entity_changes (revision numeric(20,0) PRIMARY KEY, payload bytea NOT NULL);
CREATE TABLE IF NOT EXISTS receipts (
 domain smallint NOT NULL CHECK(domain IN (1,2)), binding bytea NOT NULL,
 request bytea NOT NULL, outcome bytea NOT NULL,
 PRIMARY KEY(domain,binding)
);
CREATE TABLE IF NOT EXISTS history_points (
 id text PRIMARY KEY, incarnation bytea NOT NULL CHECK(octet_length(incarnation)=16),
 generation numeric(20,0) NOT NULL CHECK(generation >= 0), evicted bytea
);
CREATE TABLE IF NOT EXISTS history_samples (
 point text NOT NULL REFERENCES history_points(id), seconds bigint NOT NULL,
 nanos integer NOT NULL CHECK(nanos >= 0 AND nanos < 1000000000), payload bytea NOT NULL,
 PRIMARY KEY(point,seconds,nanos)
);
CREATE TABLE IF NOT EXISTS history_changes (sequence numeric(20,0) PRIMARY KEY, payload bytea NOT NULL);
"#;

pub(crate) struct StoredReceipt {
    pub(crate) request: Vec<u8>,
    pub(crate) outcome: Vec<u8>,
}
impl Backend {
    pub(crate) async fn lookup_receipt(
        &self,
        domain: i16,
        binding: &[u8],
    ) -> Result<Option<StoredReceipt>, PostgresError> {
        let mut transaction = self
            .read_transaction(
                Instant::now() + self.limits.acquire_timeout + self.limits.statement_timeout,
            )
            .await?;
        let result=tokio::time::timeout(self.limits.statement_timeout,
            sqlx::query("SELECT CASE WHEN octet_length(request) <= 1048576 THEN request END AS request, CASE WHEN octet_length(outcome) <= 1048576 THEN outcome END AS outcome FROM receipts WHERE domain=$1 AND binding=$2")
            .bind(domain).bind(binding).fetch_optional(&mut *transaction)).await;
        transaction.rollback().await?;
        let row = result
            .map_err(|_| PostgresError::Deadline)?
            .map_err(|_| PostgresError::Unavailable)?;
        row.map(|r| {
            Ok(StoredReceipt {
                request: r
                    .try_get("request")
                    .map_err(|_| PostgresError::Incompatible)?,
                outcome: r
                    .try_get("outcome")
                    .map_err(|_| PostgresError::Incompatible)?,
            })
        })
        .transpose()
    }
    pub(crate) async fn reserve_receipt(
        &self,
        domain: i16,
        binding: &[u8],
        request: &[u8],
        pending: &[u8],
        deadline: Instant,
    ) -> Result<Option<StoredReceipt>, PostgresError> {
        let mut tx = self.transaction(deadline).await?;
        let result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
        let existing = sqlx::query("SELECT CASE WHEN octet_length(request)<=1048576 THEN request END AS request, CASE WHEN octet_length(outcome)<=1048576 THEN outcome END AS outcome FROM receipts WHERE domain=$1 AND binding=$2")
            .bind(domain).bind(binding).fetch_optional(&mut *tx).await.map_err(|_| PostgresError::Unavailable)?;
        if let Some(row) = existing {
            let result = StoredReceipt {request: row.try_get("request").map_err(|_| PostgresError::Incompatible)?, outcome: row.try_get("outcome").map_err(|_| PostgresError::Incompatible)?};
            return Ok(Some(result));
        }
        let counts = sqlx::query("SELECT count(*) AS n, coalesce(sum(octet_length(binding)+octet_length(request)+octet_length(outcome)),0)::bigint AS bytes FROM receipts WHERE domain=$1")
            .bind(domain).fetch_one(&mut *tx).await.map_err(|_| PostgresError::Unavailable)?;
        let bytes = binding.len().saturating_add(request.len()).saturating_add(pending.len()).saturating_add(8192);
        if counts.try_get::<i64,_>("n").map_err(|_| PostgresError::Incompatible)? >= self.limits.max_receipts as i64
            || counts.try_get::<i64,_>("bytes").map_err(|_| PostgresError::Incompatible)?.saturating_add(bytes as i64) > self.limits.max_receipt_bytes as i64
        { return Err(PostgresError::Limit); }
        sqlx::query("INSERT INTO receipts(domain,binding,request,outcome) VALUES($1,$2,$3,$4)")
            .bind(domain).bind(binding).bind(request).bind(pending).execute(&mut *tx).await.map_err(|_| PostgresError::Unavailable)?;
        Ok(None)
        }).await.unwrap_or(Err(PostgresError::Deadline));
        match result {
            Ok(None) => {
                tx.commit().await.map_err(|_| PostgresError::Unavailable)?;
                Ok(None)
            }
            other => {
                tx.rollback()
                    .await
                    .map_err(|_| PostgresError::Unavailable)?;
                other
            }
        }
    }
    /// The FOR UPDATE serialization wait must complete before the following
    /// fresh READ COMMITTED statement can settle pending/absent state. A plain
    /// lookup is deliberately insufficient to prove an old COMMIT did not land.
    pub(crate) async fn settle_receipt(
        &self,
        domain: i16,
        binding: &[u8],
        pending: &[u8],
        rejected: &[u8],
        deadline: Instant,
    ) -> Result<Option<StoredReceipt>, PostgresError> {
        let mut tx = self.transaction(deadline).await?;
        let prepared = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
        let row = sqlx::query("SELECT CASE WHEN octet_length(request)<=1048576 THEN request END AS request, CASE WHEN octet_length(outcome)<=1048576 THEN outcome END AS outcome FROM receipts WHERE domain=$1 AND binding=$2")
            .bind(domain).bind(binding).fetch_optional(&mut *tx).await.map_err(|_| PostgresError::Unavailable)?;
        let result = if let Some(row) = row {
            let mut result = StoredReceipt {request: row.try_get("request").map_err(|_| PostgresError::Incompatible)?, outcome: row.try_get("outcome").map_err(|_| PostgresError::Incompatible)?};
            if result.outcome == pending {
                sqlx::query("UPDATE receipts SET outcome=$3 WHERE domain=$1 AND binding=$2")
                    .bind(domain).bind(binding).bind(rejected).execute(&mut *tx).await.map_err(|_| PostgresError::Unavailable)?;
                result.outcome = rejected.to_owned();
            }
            Some(result)
        } else {None};
        Ok(result)
        }).await.unwrap_or(Err(PostgresError::Deadline));
        match prepared {
            Ok(result) => {
                tx.commit().await.map_err(|_| PostgresError::Unavailable)?;
                Ok(result)
            }
            Err(error) => {
                tx.rollback()
                    .await
                    .map_err(|_| PostgresError::Unavailable)?;
                Err(error)
            }
        }
    }
}

/// A transaction owns the actual pool permit through acknowledged cleanup.
/// Closing each connection is deliberate in this first bounded profile: an
/// error or cancellation cannot release an application lease while SQLx merely
/// queues a rollback or pool-return task. The fallback Drop closes, never reuses.
pub(crate) struct OwnedTransaction {
    connection: Option<sqlx::pool::PoolConnection<Postgres>>,
    timeout: std::time::Duration,
}
impl OwnedTransaction {
    pub(crate) async fn begin(
        mut connection: sqlx::pool::PoolConnection<Postgres>,
        snapshot: bool,
        timeout: std::time::Duration,
    ) -> Result<Self, PostgresError> {
        connection.close_on_drop();
        let statement = if snapshot {
            "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY"
        } else {
            "BEGIN ISOLATION LEVEL READ COMMITTED READ WRITE"
        };
        if !matches!(
            tokio::time::timeout(timeout, sqlx::query(statement).execute(&mut *connection)).await,
            Ok(Ok(_))
        ) {
            let _ = tokio::time::timeout(timeout, connection.close()).await;
            return Err(PostgresError::Unavailable);
        }
        Ok(Self {
            connection: Some(connection),
            timeout,
        })
    }
    async fn finish(mut self, commit: bool) -> Result<(), PostgresError> {
        let mut connection = self
            .connection
            .take()
            .expect("owned transaction connection");
        let result = tokio::time::timeout(
            self.timeout,
            sqlx::query(if commit { "COMMIT" } else { "ROLLBACK" }).execute(&mut *connection),
        )
        .await;
        let closed = tokio::time::timeout(self.timeout, connection.close()).await;
        // A COMMIT error stays uncertain even if the subsequent close succeeds.
        result
            .map_err(|_| PostgresError::Deadline)?
            .map_err(|_| PostgresError::Unavailable)?;
        closed
            .map_err(|_| PostgresError::Deadline)?
            .map_err(|_| PostgresError::Unavailable)?;
        Ok(())
    }
    pub(crate) async fn commit(self) -> Result<(), PostgresError> {
        self.finish(true).await
    }
    pub(crate) async fn rollback(self) -> Result<(), PostgresError> {
        self.finish(false).await
    }
}
impl std::ops::Deref for OwnedTransaction {
    type Target = PgConnection;
    fn deref(&self) -> &Self::Target {
        self.connection
            .as_ref()
            .expect("owned transaction connection")
    }
}
impl std::ops::DerefMut for OwnedTransaction {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.connection
            .as_mut()
            .expect("owned transaction connection")
    }
}

#[cfg(test)]
pub(crate) enum CommitFault {
    LoseAcknowledgment,
    SealBeforePublication,
    TerminateBeforeCommit,
    PauseAfterCommit {
        entered: tokio::sync::oneshot::Sender<()>,
        release: tokio::sync::oneshot::Receiver<()>,
    },
    HoldUncertain {
        entered: tokio::sync::oneshot::Sender<()>,
        release: tokio::sync::oneshot::Receiver<()>,
        completed: tokio::sync::oneshot::Sender<Result<(), PostgresError>>,
    },
}
impl Backend {
    pub(crate) async fn commit_mutation(
        &self,
        tx: OwnedTransaction,
        _lease: Option<Arc<crate::service::WorkLease>>,
    ) -> Result<(), PostgresError> {
        #[cfg(test)]
        {
            let fault = self.fault.lock().take();
            match fault {
                Some(CommitFault::LoseAcknowledgment) => {
                    tx.commit().await?;
                    return Err(PostgresError::Unavailable);
                }
                Some(CommitFault::SealBeforePublication) => {
                    tx.commit().await?;
                    self.seal();
                    return Err(PostgresError::Unavailable);
                }
                Some(CommitFault::PauseAfterCommit { entered, release }) => {
                    tx.commit().await?;
                    let _ = entered.send(());
                    let _ = tokio::time::timeout(self.limits.statement_timeout, release).await;
                    return Ok(());
                }
                Some(CommitFault::TerminateBeforeCommit) => {
                    let mut tx = tx;
                    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(|_| PostgresError::Unavailable)?;
                    let _: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
                        .bind(pid)
                        .fetch_one(&self.pool)
                        .await
                        .map_err(|_| PostgresError::Unavailable)?;
                    return tx.commit().await;
                }
                Some(CommitFault::HoldUncertain {
                    entered,
                    release,
                    completed,
                }) => {
                    let mut tx = tx;
                    let revision: String =
                        sqlx::query_scalar("SELECT revision::text FROM metadata WHERE singleton=1")
                            .fetch_one(&mut *tx)
                            .await
                            .map_err(|_| PostgresError::Unavailable)?;
                    self.provisional_revision
                        .store(super::value::number(&revision)?, Ordering::SeqCst);
                    let timeout = self.limits.transaction_timeout;
                    self.runtime.spawn(async move {
                        let _owned_lease = _lease;
                        let _ = entered.send(());
                        let result = match tokio::time::timeout(timeout, release).await {
                            Ok(Ok(())) => tx.commit().await,
                            _ => tx.rollback().await,
                        };
                        drop(_owned_lease);
                        let _ = completed.send(result);
                    });
                    return Err(PostgresError::Unavailable);
                }
                None => {}
            }
        }
        tx.commit().await
    }
}

/// Connection interruptions are availability failures. Only a proven storage
/// shape/type mismatch fences the projection; reconnect must remain possible.
pub(crate) fn read_error(error: sqlx::Error) -> PostgresError {
    if matches!(error, sqlx::Error::RowNotFound)
        || error
            .as_database_error()
            .and_then(|e| e.code())
            .is_some_and(|code| code.starts_with("42") || code.starts_with("22"))
    {
        PostgresError::Incompatible
    } else {
        PostgresError::Unavailable
    }
}
#[cfg(test)]
pub(crate) struct Acquisition<'a>(pub(crate) &'a std::sync::atomic::AtomicUsize);
#[cfg(test)]
impl<'a> Acquisition<'a> {
    pub(crate) fn new(counter: &'a std::sync::atomic::AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}
#[cfg(test)]
impl Drop for Acquisition<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshStage {
    AfterEntities,
    BeforePublication,
}
#[cfg(test)]
pub(crate) struct RefreshFault {
    pub(crate) stage: RefreshStage,
    pub(crate) entered: tokio::sync::oneshot::Sender<()>,
    pub(crate) release: tokio::sync::oneshot::Receiver<()>,
}
#[cfg(test)]
impl Backend {
    pub(crate) async fn refresh_barrier(&self, stage: RefreshStage) -> Result<(), PostgresError> {
        let fault = {
            let mut fault = self.refresh_fault.lock();
            if fault.as_ref().is_some_and(|fault| fault.stage == stage) {
                fault.take()
            } else {
                None
            }
        };
        if let Some(fault) = fault {
            let _ = fault.entered.send(());
            tokio::time::timeout(self.limits.transaction_timeout, fault.release)
                .await
                .map_err(|_| PostgresError::Deadline)?
                .map_err(|_| PostgresError::Unavailable)?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) struct SetupCloseFault {
    pub(crate) entered: tokio::sync::oneshot::Sender<()>,
    pub(crate) release: tokio::sync::oneshot::Receiver<()>,
}

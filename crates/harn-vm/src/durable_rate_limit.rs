use crate::value::VmDictExt;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::{params, Connection, ErrorCode, TransactionBehavior};

use crate::runtime_sqlite::{initialize_runtime_sqlite, RuntimeSqliteError, RuntimeSqliteSchema};
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::stdlib::options::{non_negative_millis_from_value, ErrorKind};
use crate::stdlib::sandbox::{self, FsAccess};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const DEFAULT_WINDOW_MS: u64 = 60_000;
const DEFAULT_BUSY_TIMEOUT_MS: u64 = 5_000;
const SQLITE_BUSY_RETRY_INITIAL_MS: u64 = 2;
const SQLITE_BUSY_RETRY_MAX_MS: u64 = 50;
const MAX_SLEEP_MS: u64 = 60_000;
const FAIR_QUEUE_POLL_MS: u64 = 250;
const FAIR_QUEUE_MIN_STALE_MS: u64 = 120_000;
const SQLITE_SCHEMA: RuntimeSqliteSchema = RuntimeSqliteSchema::new(
    "durable_rate_limit",
    2,
    "CREATE TABLE IF NOT EXISTS durable_rate_limit_entries (
        bucket_key TEXT NOT NULL,
        ts_ms INTEGER NOT NULL,
        units INTEGER NOT NULL CHECK(units >= 0)
    );
    CREATE INDEX IF NOT EXISTS durable_rate_limit_entries_key_ts_idx
        ON durable_rate_limit_entries(bucket_key, ts_ms);
    CREATE TABLE IF NOT EXISTS durable_rate_limit_waiters (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        queue_key TEXT NOT NULL,
        consumer_id TEXT NOT NULL,
        enqueued_at_ms INTEGER NOT NULL,
        heartbeat_ms INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS durable_rate_limit_waiters_queue_idx
        ON durable_rate_limit_waiters(queue_key, enqueued_at_ms, id);
    CREATE TABLE IF NOT EXISTS durable_rate_limit_consumers (
        queue_key TEXT NOT NULL,
        consumer_id TEXT NOT NULL,
        served_count INTEGER NOT NULL DEFAULT 0,
        queued_count INTEGER NOT NULL DEFAULT 0,
        rerouted_count INTEGER NOT NULL DEFAULT 0,
        last_served_seq INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY(queue_key, consumer_id)
    );
    CREATE TABLE IF NOT EXISTS durable_rate_limit_queue_state (
        queue_key TEXT PRIMARY KEY,
        served_seq INTEGER NOT NULL DEFAULT 0
    );",
);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RateBucket {
    key: String,
    limit: u64,
    units: u64,
    charged_units: u64,
    window_ms: u64,
}

impl RateBucket {
    pub(crate) fn new(key: String, limit: u64, units: u64, window_ms: u64) -> Self {
        let charged_units = if units == 0 { 0 } else { units.min(limit) };
        Self {
            key,
            limit,
            units,
            charged_units,
            window_ms,
        }
    }

    pub(crate) fn key(&self) -> &str {
        &self.key
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ReserveAttempt {
    acquired: bool,
    retry_after_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DurableRateLimitOutcome {
    pub(crate) acquired: bool,
    pub(crate) timed_out: bool,
    pub(crate) waited_ms: u64,
    pub(crate) retry_after_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FairRateLimitCounters {
    pub(crate) served: u64,
    pub(crate) queued: u64,
    pub(crate) rerouted: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FairRateLimitSnapshot {
    pub(crate) queue_position: u64,
    pub(crate) expected_wait_ms: u64,
    pub(crate) counters: FairRateLimitCounters,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DurableFairRateLimitOutcome {
    pub(crate) acquired: bool,
    pub(crate) timed_out: bool,
    pub(crate) waited_ms: u64,
    pub(crate) retry_after_ms: u64,
    pub(crate) counters: FairRateLimitCounters,
}

#[derive(Debug, PartialEq, Eq)]
struct FairReserveAttempt {
    acquired: bool,
    ticket_id: Option<i64>,
    snapshot: Option<FairRateLimitSnapshot>,
    retry_after_ms: u64,
    counters: FairRateLimitCounters,
}

struct FairWaiterGuard {
    state_path: PathBuf,
    queue_key: String,
    consumer_id: String,
    ticket_id: Option<i64>,
}

impl FairWaiterGuard {
    fn new(state_path: PathBuf, queue_key: String, consumer_id: String) -> Self {
        Self {
            state_path,
            queue_key,
            consumer_id,
            ticket_id: None,
        }
    }
}

impl Drop for FairWaiterGuard {
    fn drop(&mut self) {
        let Some(ticket_id) = self.ticket_id else {
            return;
        };
        // Forced task abort drops the admission future without reaching its
        // async cancellation branch. Best-effort synchronous cleanup keeps
        // that abandoned ticket from blocking every live consumer; heartbeat
        // expiry remains the process-crash backstop.
        let _ = remove_fair_waiter_sync(
            &self.state_path,
            &self.queue_key,
            &self.consumer_id,
            ticket_id,
            false,
        );
    }
}

pub(crate) fn register_durable_rate_limit_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    sig = "durable_rate_limit_acquire(options: dict) -> dict",
    kind = "async",
    category = "concurrency",
    doc = "Reserve one or more durable sliding-window quota buckets across processes."
)]
async fn durable_rate_limit_acquire_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let vm = ctx.child_vm();
    let options = args.first().and_then(VmValue::as_dict).ok_or_else(|| {
        VmError::Runtime("durable_rate_limit_acquire: options dict is required".to_string())
    })?;
    let state_path = parse_state_path(options)?;
    let buckets = parse_buckets(options)?;
    let timeout_ms = optional_duration_ms(options, "timeout_ms")?;

    sandbox::enforce_fs_path("durable_rate_limit_acquire", &state_path, FsAccess::Write)?;

    let outcome =
        acquire_durable_rate_limit(state_path.clone(), buckets.clone(), timeout_ms, || {
            vm.cancel_token
                .as_ref()
                .is_some_and(|token| token.load(std::sync::atomic::Ordering::SeqCst))
        })
        .await?;

    Ok(result_value(
        outcome.acquired,
        outcome.timed_out,
        outcome.waited_ms,
        outcome.retry_after_ms,
        &state_path,
        &buckets,
    ))
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[&DURABLE_RATE_LIMIT_ACQUIRE_IMPL_DEF];

fn parse_state_path(options: &crate::value::DictMap) -> Result<PathBuf, VmError> {
    match options.get("state_path") {
        Some(VmValue::String(path)) if !path.trim().is_empty() => Ok(
            crate::stdlib::process::resolve_source_relative_path(path.trim()),
        ),
        Some(VmValue::Nil) | None => {
            let base = crate::stdlib::process::runtime_root_base();
            Ok(crate::runtime_paths::state_root(&base).join("rate-limits.sqlite"))
        }
        Some(other) => Err(VmError::Runtime(format!(
            "durable_rate_limit_acquire: state_path must be a string or nil (got {})",
            other.type_name()
        ))),
    }
}

fn parse_buckets(options: &crate::value::DictMap) -> Result<Vec<RateBucket>, VmError> {
    let buckets = match options.get("buckets") {
        Some(VmValue::List(items)) => {
            let mut parsed = Vec::with_capacity(items.len());
            for item in items.iter() {
                let dict = item.as_dict().ok_or_else(|| {
                    VmError::Runtime(
                        "durable_rate_limit_acquire: each bucket must be a dict".to_string(),
                    )
                })?;
                parsed.push(parse_bucket(dict)?);
            }
            parsed
        }
        Some(VmValue::Nil) | None => vec![parse_bucket(options)?],
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "durable_rate_limit_acquire: buckets must be a list or nil (got {})",
                other.type_name()
            )));
        }
    };

    if buckets.is_empty() {
        return Err(VmError::Runtime(
            "durable_rate_limit_acquire: at least one bucket is required".to_string(),
        ));
    }

    let mut seen = BTreeSet::new();
    for bucket in &buckets {
        if !seen.insert(bucket.key.clone()) {
            return Err(VmError::Runtime(format!(
                "durable_rate_limit_acquire: duplicate bucket key `{}`",
                bucket.key
            )));
        }
    }
    Ok(buckets)
}

fn parse_bucket(dict: &crate::value::DictMap) -> Result<RateBucket, VmError> {
    let key = required_string_field(dict, "key")?;
    let limit = required_positive_u64_field(dict, "limit")?;
    let units = optional_non_negative_u64_field(dict, "units")?.unwrap_or(1);
    let window_ms = optional_duration_ms(dict, "window_ms")?.unwrap_or(DEFAULT_WINDOW_MS);
    if window_ms == 0 {
        return Err(VmError::Runtime(
            "durable_rate_limit_acquire: bucket.window_ms must be positive".to_string(),
        ));
    }
    Ok(RateBucket::new(key, limit, units, window_ms))
}

fn required_string_field(
    dict: &crate::value::DictMap,
    key: &'static str,
) -> Result<String, VmError> {
    match dict.get(key) {
        Some(VmValue::String(value)) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        Some(value) => Err(VmError::Runtime(format!(
            "durable_rate_limit_acquire: bucket.{key} must be a non-empty string (got {})",
            value.type_name()
        ))),
        None => Err(VmError::Runtime(format!(
            "durable_rate_limit_acquire: bucket.{key} is required"
        ))),
    }
}

fn required_positive_u64_field(
    dict: &crate::value::DictMap,
    key: &'static str,
) -> Result<u64, VmError> {
    let value = optional_non_negative_u64_field(dict, key)?.ok_or_else(|| {
        VmError::Runtime(format!(
            "durable_rate_limit_acquire: bucket.{key} is required"
        ))
    })?;
    if value == 0 {
        return Err(VmError::Runtime(format!(
            "durable_rate_limit_acquire: bucket.{key} must be positive"
        )));
    }
    Ok(value)
}

fn optional_non_negative_u64_field(
    dict: &crate::value::DictMap,
    key: &'static str,
) -> Result<Option<u64>, VmError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(value) => {
            let Some(raw) = value.as_int() else {
                return Err(VmError::Runtime(format!(
                    "durable_rate_limit_acquire: bucket.{key} must be an integer"
                )));
            };
            if raw < 0 {
                return Err(VmError::Runtime(format!(
                    "durable_rate_limit_acquire: bucket.{key} must be non-negative"
                )));
            }
            Ok(Some(raw as u64))
        }
    }
}

fn optional_duration_ms(
    dict: &crate::value::DictMap,
    key: &'static str,
) -> Result<Option<u64>, VmError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(value) => non_negative_millis_from_value(
            value,
            "durable_rate_limit_acquire",
            key,
            ErrorKind::Runtime,
        )
        .map(Some),
    }
}

fn try_reserve_once(
    path: &Path,
    buckets: &[RateBucket],
    now_ms: i64,
) -> Result<ReserveAttempt, VmError> {
    try_reserve_once_with_options(
        path,
        buckets,
        now_ms,
        DEFAULT_BUSY_TIMEOUT_MS,
        DEFAULT_BUSY_TIMEOUT_MS,
        || {},
    )
}

fn try_reserve_once_with_options<F>(
    path: &Path,
    buckets: &[RateBucket],
    now_ms: i64,
    sqlite_busy_timeout_ms: u64,
    retry_timeout_ms: u64,
    mut on_busy: F,
) -> Result<ReserveAttempt, VmError>
where
    F: FnMut(),
{
    let started = Instant::now();
    let mut backoff = Duration::from_millis(SQLITE_BUSY_RETRY_INITIAL_MS);
    let retry_timeout = Duration::from_millis(retry_timeout_ms);
    loop {
        match try_reserve_once_inner(path, buckets, now_ms, sqlite_busy_timeout_ms) {
            Ok(attempt) => return Ok(attempt),
            Err(error) if error.is_sqlite_busy_or_locked() && started.elapsed() < retry_timeout => {
                on_busy();
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_millis(SQLITE_BUSY_RETRY_MAX_MS));
            }
            Err(error) => return Err(error.into_vm_error()),
        }
    }
}

#[derive(Debug)]
enum ReserveOnceError {
    Vm(VmError),
    Sqlite(rusqlite::Error),
    RuntimeSqlite(RuntimeSqliteError),
}

impl ReserveOnceError {
    fn is_sqlite_busy_or_locked(&self) -> bool {
        match self {
            Self::Sqlite(rusqlite::Error::SqliteFailure(error, _)) => {
                matches!(
                    error.code,
                    ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
                )
            }
            Self::RuntimeSqlite(error) => error.is_busy_or_locked(),
            Self::Vm(_) | Self::Sqlite(_) => false,
        }
    }

    fn into_vm_error(self) -> VmError {
        match self {
            Self::Vm(error) => error,
            Self::Sqlite(error) => sql_error(error),
            Self::RuntimeSqlite(error) => VmError::Runtime(format!(
                "durable_rate_limit_acquire: sqlite setup error: {error}"
            )),
        }
    }
}

impl From<rusqlite::Error> for ReserveOnceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<VmError> for ReserveOnceError {
    fn from(error: VmError) -> Self {
        Self::Vm(error)
    }
}

fn try_reserve_once_inner(
    path: &Path,
    buckets: &[RateBucket],
    now_ms: i64,
    sqlite_busy_timeout_ms: u64,
) -> Result<ReserveAttempt, ReserveOnceError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| {
            VmError::Runtime(format!(
                "durable_rate_limit_acquire: could not create {}: {error}",
                parent.display()
            ))
        })?;
    }

    let mut conn = Connection::open(path)?;
    initialize_runtime_sqlite(
        &conn,
        Duration::from_millis(sqlite_busy_timeout_ms),
        &SQLITE_SCHEMA,
    )
    .map_err(ReserveOnceError::RuntimeSqlite)?;

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut retry_after_ms = 0_u64;
    for bucket in buckets {
        prune_bucket(&tx, bucket, now_ms)?;
        if bucket.charged_units == 0 {
            continue;
        }
        if let Some(wait_ms) = bucket_wait_ms(&tx, bucket, now_ms)? {
            retry_after_ms = retry_after_ms.max(wait_ms);
        }
    }

    if retry_after_ms > 0 {
        tx.commit()?;
        return Ok(ReserveAttempt {
            acquired: false,
            retry_after_ms,
        });
    }

    for bucket in buckets {
        if bucket.charged_units == 0 {
            continue;
        }
        tx.execute(
            "INSERT INTO durable_rate_limit_entries (bucket_key, ts_ms, units)
             VALUES (?1, ?2, ?3)",
            params![
                &bucket.key,
                now_ms,
                i64::try_from(bucket.charged_units).unwrap_or(i64::MAX)
            ],
        )
        .map_err(ReserveOnceError::Sqlite)?;
    }
    tx.commit()?;
    Ok(ReserveAttempt {
        acquired: true,
        retry_after_ms: 0,
    })
}

fn try_reserve_fair_once(
    path: &Path,
    buckets: &[RateBucket],
    queue_key: &str,
    consumer_id: &str,
    ticket_id: Option<i64>,
    now_ms: i64,
    starvation_ms: u64,
) -> Result<FairReserveAttempt, VmError> {
    let started = Instant::now();
    let mut backoff = Duration::from_millis(SQLITE_BUSY_RETRY_INITIAL_MS);
    let retry_timeout = Duration::from_millis(DEFAULT_BUSY_TIMEOUT_MS);
    loop {
        match try_reserve_fair_once_inner(
            path,
            buckets,
            queue_key,
            consumer_id,
            ticket_id,
            now_ms,
            starvation_ms,
            DEFAULT_BUSY_TIMEOUT_MS,
        ) {
            Ok(attempt) => return Ok(attempt),
            Err(error) if error.is_sqlite_busy_or_locked() && started.elapsed() < retry_timeout => {
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_millis(SQLITE_BUSY_RETRY_MAX_MS));
            }
            Err(error) => return Err(error.into_vm_error()),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn try_reserve_fair_once_inner(
    path: &Path,
    buckets: &[RateBucket],
    queue_key: &str,
    consumer_id: &str,
    mut ticket_id: Option<i64>,
    now_ms: i64,
    starvation_ms: u64,
    sqlite_busy_timeout_ms: u64,
) -> Result<FairReserveAttempt, ReserveOnceError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| {
            VmError::Runtime(format!(
                "durable_rate_limit_acquire: could not create {}: {error}",
                parent.display()
            ))
        })?;
    }

    let mut conn = Connection::open(path)?;
    initialize_runtime_sqlite(
        &conn,
        Duration::from_millis(sqlite_busy_timeout_ms),
        &SQLITE_SCHEMA,
    )
    .map_err(ReserveOnceError::RuntimeSqlite)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stale_ms = buckets
        .iter()
        .map(|bucket| bucket.window_ms.saturating_mul(2))
        .max()
        .unwrap_or(FAIR_QUEUE_MIN_STALE_MS)
        .max(FAIR_QUEUE_MIN_STALE_MS);
    tx.execute(
        "DELETE FROM durable_rate_limit_waiters
         WHERE queue_key = ?1 AND heartbeat_ms < ?2",
        params![queue_key, now_ms.saturating_sub(u64_to_i64(stale_ms))],
    )?;
    ensure_fair_consumer(&tx, queue_key, consumer_id)?;

    let mut retry_after_ms = bucket_retry_after(&tx, buckets, now_ms)?;
    let queued_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM durable_rate_limit_waiters WHERE queue_key = ?1",
        params![queue_key],
        |row| row.get(0),
    )?;

    if ticket_id.is_none() && queued_count == 0 && retry_after_ms == 0 {
        record_buckets(&tx, buckets, now_ms)?;
        record_fair_served(&tx, queue_key, consumer_id)?;
        let counters = fair_counters(&tx, queue_key, consumer_id)?;
        tx.commit()?;
        return Ok(FairReserveAttempt {
            acquired: true,
            ticket_id: None,
            snapshot: None,
            retry_after_ms: 0,
            counters,
        });
    }

    if ticket_id.is_none() {
        tx.execute(
            "INSERT INTO durable_rate_limit_waiters
             (queue_key, consumer_id, enqueued_at_ms, heartbeat_ms)
             VALUES (?1, ?2, ?3, ?3)",
            params![queue_key, consumer_id, now_ms],
        )?;
        ticket_id = Some(tx.last_insert_rowid());
        tx.execute(
            "UPDATE durable_rate_limit_consumers
             SET queued_count = queued_count + 1
             WHERE queue_key = ?1 AND consumer_id = ?2",
            params![queue_key, consumer_id],
        )?;
    }
    let mut ticket_id = ticket_id.expect("fair queue ticket was inserted");
    let heartbeat_rows = tx.execute(
        "UPDATE durable_rate_limit_waiters SET heartbeat_ms = ?2
         WHERE id = ?1 AND queue_key = ?3",
        params![ticket_id, now_ms, queue_key],
    )?;
    if heartbeat_rows == 0 {
        // A process may resume after crash-recovery pruning removed its stale
        // ticket. Rejoin at the tail instead of polling a nonexistent id.
        tx.execute(
            "INSERT INTO durable_rate_limit_waiters
             (queue_key, consumer_id, enqueued_at_ms, heartbeat_ms)
             VALUES (?1, ?2, ?3, ?3)",
            params![queue_key, consumer_id, now_ms],
        )?;
        ticket_id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE durable_rate_limit_consumers
             SET queued_count = queued_count + 1
             WHERE queue_key = ?1 AND consumer_id = ?2",
            params![queue_key, consumer_id],
        )?;
    }

    let ordered = ordered_waiter_ids(&tx, queue_key, now_ms, starvation_ms)?;
    let position = ordered
        .iter()
        .position(|id| *id == ticket_id)
        .map(|index| index as u64 + 1)
        .unwrap_or(1);
    let selected = ordered.first().copied() == Some(ticket_id);

    if selected {
        retry_after_ms = bucket_retry_after(&tx, buckets, now_ms)?;
        if retry_after_ms == 0 {
            record_buckets(&tx, buckets, now_ms)?;
            tx.execute(
                "DELETE FROM durable_rate_limit_waiters WHERE id = ?1",
                params![ticket_id],
            )?;
            record_fair_served(&tx, queue_key, consumer_id)?;
            let counters = fair_counters(&tx, queue_key, consumer_id)?;
            tx.commit()?;
            return Ok(FairReserveAttempt {
                acquired: true,
                ticket_id: None,
                snapshot: None,
                retry_after_ms: 0,
                counters,
            });
        }
    } else {
        retry_after_ms = retry_after_ms.max(FAIR_QUEUE_POLL_MS);
    }

    let counters = fair_counters(&tx, queue_key, consumer_id)?;
    let snapshot = FairRateLimitSnapshot {
        queue_position: position,
        expected_wait_ms: retry_after_ms.max(1).saturating_mul(position),
        counters: counters.clone(),
    };
    tx.commit()?;
    Ok(FairReserveAttempt {
        acquired: false,
        ticket_id: Some(ticket_id),
        snapshot: Some(snapshot),
        retry_after_ms: retry_after_ms.max(1),
        counters,
    })
}

fn ensure_fair_consumer(
    tx: &rusqlite::Transaction<'_>,
    queue_key: &str,
    consumer_id: &str,
) -> Result<(), ReserveOnceError> {
    tx.execute(
        "INSERT OR IGNORE INTO durable_rate_limit_consumers
         (queue_key, consumer_id) VALUES (?1, ?2)",
        params![queue_key, consumer_id],
    )?;
    Ok(())
}

fn ordered_waiter_ids(
    tx: &rusqlite::Transaction<'_>,
    queue_key: &str,
    now_ms: i64,
    starvation_ms: u64,
) -> Result<Vec<i64>, ReserveOnceError> {
    let starvation_cutoff = now_ms.saturating_sub(u64_to_i64(starvation_ms));
    let mut stmt = tx.prepare(
        "SELECT w.id
         FROM durable_rate_limit_waiters w
         LEFT JOIN durable_rate_limit_consumers c
           ON c.queue_key = w.queue_key AND c.consumer_id = w.consumer_id
         WHERE w.queue_key = ?1
         ORDER BY
           CASE WHEN w.enqueued_at_ms <= ?2 THEN 0 ELSE 1 END,
           COALESCE(c.last_served_seq, 0),
           w.enqueued_at_ms,
           w.id",
    )?;
    let ids = stmt
        .query_map(params![queue_key, starvation_cutoff], |row| row.get(0))?
        .collect::<Result<Vec<i64>, _>>()?;
    Ok(ids)
}

fn bucket_retry_after(
    tx: &rusqlite::Transaction<'_>,
    buckets: &[RateBucket],
    now_ms: i64,
) -> Result<u64, ReserveOnceError> {
    let mut retry_after_ms = 0;
    for bucket in buckets {
        prune_bucket(tx, bucket, now_ms)?;
        if bucket.charged_units == 0 {
            continue;
        }
        if let Some(wait_ms) = bucket_wait_ms(tx, bucket, now_ms)? {
            retry_after_ms = retry_after_ms.max(wait_ms);
        }
    }
    Ok(retry_after_ms)
}

fn record_buckets(
    tx: &rusqlite::Transaction<'_>,
    buckets: &[RateBucket],
    now_ms: i64,
) -> Result<(), ReserveOnceError> {
    for bucket in buckets {
        if bucket.charged_units == 0 {
            continue;
        }
        tx.execute(
            "INSERT INTO durable_rate_limit_entries (bucket_key, ts_ms, units)
             VALUES (?1, ?2, ?3)",
            params![
                &bucket.key,
                now_ms,
                i64::try_from(bucket.charged_units).unwrap_or(i64::MAX)
            ],
        )?;
    }
    Ok(())
}

fn record_fair_served(
    tx: &rusqlite::Transaction<'_>,
    queue_key: &str,
    consumer_id: &str,
) -> Result<(), ReserveOnceError> {
    tx.execute(
        "INSERT OR IGNORE INTO durable_rate_limit_queue_state (queue_key) VALUES (?1)",
        params![queue_key],
    )?;
    let sequence: i64 = tx.query_row(
        "SELECT served_seq + 1 FROM durable_rate_limit_queue_state WHERE queue_key = ?1",
        params![queue_key],
        |row| row.get(0),
    )?;
    tx.execute(
        "UPDATE durable_rate_limit_queue_state SET served_seq = ?2 WHERE queue_key = ?1",
        params![queue_key, sequence],
    )?;
    tx.execute(
        "UPDATE durable_rate_limit_consumers
         SET served_count = served_count + 1, last_served_seq = ?3
         WHERE queue_key = ?1 AND consumer_id = ?2",
        params![queue_key, consumer_id, sequence],
    )?;
    Ok(())
}

fn fair_counters(
    tx: &rusqlite::Transaction<'_>,
    queue_key: &str,
    consumer_id: &str,
) -> Result<FairRateLimitCounters, ReserveOnceError> {
    let (served, queued, rerouted): (i64, i64, i64) = tx.query_row(
        "SELECT served_count, queued_count, rerouted_count
         FROM durable_rate_limit_consumers
         WHERE queue_key = ?1 AND consumer_id = ?2",
        params![queue_key, consumer_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    Ok(FairRateLimitCounters {
        served: served.max(0) as u64,
        queued: queued.max(0) as u64,
        rerouted: rerouted.max(0) as u64,
    })
}

pub(crate) async fn acquire_durable_rate_limit<F>(
    state_path: PathBuf,
    buckets: Vec<RateBucket>,
    timeout_ms: Option<u64>,
    is_cancelled: F,
) -> Result<DurableRateLimitOutcome, VmError>
where
    F: Fn() -> bool,
{
    let started_ms = now_wall_ms();
    let mut waited_ms = 0_u64;
    loop {
        if is_cancelled() {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "kind:cancelled:VM cancelled by host",
            ))));
        }

        let now_ms = now_wall_ms();
        let attempt_path = state_path.clone();
        let attempt_buckets = buckets.clone();
        let attempt = tokio::task::spawn_blocking(move || {
            try_reserve_once(&attempt_path, &attempt_buckets, now_ms)
        })
        .await
        .map_err(|error| {
            VmError::Runtime(format!(
                "durable_rate_limit_acquire: worker failed: {error}"
            ))
        })??;

        if attempt.acquired {
            return Ok(DurableRateLimitOutcome {
                acquired: true,
                timed_out: false,
                waited_ms,
                retry_after_ms: 0,
            });
        }

        let retry_after_ms = attempt.retry_after_ms.max(1);
        let elapsed_ms = now_ms.saturating_sub(started_ms).max(0) as u64;
        if let Some(timeout_ms) = timeout_ms {
            if elapsed_ms >= timeout_ms {
                return Ok(DurableRateLimitOutcome {
                    acquired: false,
                    timed_out: true,
                    waited_ms,
                    retry_after_ms,
                });
            }
            let remaining_ms = timeout_ms.saturating_sub(elapsed_ms);
            if retry_after_ms > remaining_ms {
                if remaining_ms > 0 {
                    sleep_ms(remaining_ms).await;
                    waited_ms = waited_ms.saturating_add(remaining_ms);
                }
                return Ok(DurableRateLimitOutcome {
                    acquired: false,
                    timed_out: true,
                    waited_ms,
                    retry_after_ms,
                });
            }
        }

        let sleep_for_ms = retry_after_ms.min(MAX_SLEEP_MS);
        sleep_ms(sleep_for_ms).await;
        waited_ms = waited_ms.saturating_add(sleep_for_ms);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn acquire_fair_durable_rate_limit<F, Q>(
    state_path: PathBuf,
    buckets: Vec<RateBucket>,
    queue_key: String,
    consumer_id: String,
    timeout_ms: Option<u64>,
    starvation_ms: u64,
    mark_rerouted_on_timeout: bool,
    is_cancelled: F,
    mut on_queued: Q,
) -> Result<DurableFairRateLimitOutcome, VmError>
where
    F: Fn() -> bool,
    Q: FnMut(&FairRateLimitSnapshot),
{
    let started_ms = now_wall_ms();
    let mut waited_ms = 0_u64;
    let mut waiter =
        FairWaiterGuard::new(state_path.clone(), queue_key.clone(), consumer_id.clone());
    let mut last_queue_position = None;
    loop {
        if is_cancelled() {
            if let Some(ticket_id) = waiter.ticket_id {
                remove_fair_waiter(
                    state_path.clone(),
                    queue_key.clone(),
                    consumer_id.clone(),
                    ticket_id,
                    false,
                )
                .await?;
                waiter.ticket_id = None;
            }
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "kind:cancelled:VM cancelled by host",
            ))));
        }

        let now_ms = now_wall_ms();
        let attempt_path = state_path.clone();
        let attempt_buckets = buckets.clone();
        let attempt_queue_key = queue_key.clone();
        let attempt_consumer_id = consumer_id.clone();
        let attempt = tokio::task::spawn_blocking(move || {
            try_reserve_fair_once(
                &attempt_path,
                &attempt_buckets,
                &attempt_queue_key,
                &attempt_consumer_id,
                waiter.ticket_id,
                now_ms,
                starvation_ms,
            )
        })
        .await
        .map_err(|error| {
            VmError::Runtime(format!(
                "durable_rate_limit_acquire: fair-queue worker failed: {error}"
            ))
        })??;

        waiter.ticket_id = attempt.ticket_id;
        if attempt.acquired {
            return Ok(DurableFairRateLimitOutcome {
                acquired: true,
                timed_out: false,
                waited_ms,
                retry_after_ms: 0,
                counters: attempt.counters,
            });
        }

        let queue_position = attempt
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.queue_position);
        if let Some(snapshot) = attempt.snapshot.as_ref() {
            if last_queue_position != Some(snapshot.queue_position) {
                on_queued(snapshot);
                last_queue_position = Some(snapshot.queue_position);
            }
        }

        let retry_after_ms = attempt.retry_after_ms.max(1);
        let elapsed_ms = now_ms.saturating_sub(started_ms).max(0) as u64;
        if let Some(timeout_ms) = timeout_ms {
            if elapsed_ms >= timeout_ms || retry_after_ms > timeout_ms.saturating_sub(elapsed_ms) {
                let remaining_ms = timeout_ms.saturating_sub(elapsed_ms);
                if remaining_ms > 0 {
                    sleep_ms(remaining_ms).await;
                    waited_ms = waited_ms.saturating_add(remaining_ms);
                }
                let counters = if let Some(ticket_id) = waiter.ticket_id {
                    let counters = remove_fair_waiter(
                        state_path,
                        queue_key,
                        consumer_id,
                        ticket_id,
                        mark_rerouted_on_timeout,
                    )
                    .await?;
                    waiter.ticket_id = None;
                    counters
                } else {
                    attempt.counters
                };
                return Ok(DurableFairRateLimitOutcome {
                    acquired: false,
                    timed_out: true,
                    waited_ms,
                    retry_after_ms,
                    counters,
                });
            }
        }

        let sleep_for_ms = if queue_position == Some(1) {
            retry_after_ms.min(MAX_SLEEP_MS)
        } else {
            retry_after_ms.min(FAIR_QUEUE_POLL_MS)
        };
        sleep_ms(sleep_for_ms).await;
        waited_ms = waited_ms.saturating_add(sleep_for_ms);
    }
}

async fn remove_fair_waiter(
    state_path: PathBuf,
    queue_key: String,
    consumer_id: String,
    ticket_id: i64,
    mark_rerouted: bool,
) -> Result<FairRateLimitCounters, VmError> {
    tokio::task::spawn_blocking(move || {
        remove_fair_waiter_sync(
            &state_path,
            &queue_key,
            &consumer_id,
            ticket_id,
            mark_rerouted,
        )
    })
    .await
    .map_err(|error| {
        VmError::Runtime(format!(
            "durable_rate_limit_acquire: fair-queue cleanup worker failed: {error}"
        ))
    })?
}

fn remove_fair_waiter_sync(
    state_path: &Path,
    queue_key: &str,
    consumer_id: &str,
    ticket_id: i64,
    mark_rerouted: bool,
) -> Result<FairRateLimitCounters, VmError> {
    let conn = Connection::open(state_path).map_err(sql_error)?;
    initialize_runtime_sqlite(
        &conn,
        Duration::from_millis(DEFAULT_BUSY_TIMEOUT_MS),
        &SQLITE_SCHEMA,
    )
    .map_err(|error| {
        VmError::Runtime(format!(
            "durable_rate_limit_acquire: sqlite setup error: {error}"
        ))
    })?;
    conn.execute(
        "DELETE FROM durable_rate_limit_waiters
         WHERE id = ?1 AND queue_key = ?2 AND consumer_id = ?3",
        params![ticket_id, queue_key, consumer_id],
    )
    .map_err(sql_error)?;
    if mark_rerouted {
        conn.execute(
            "UPDATE durable_rate_limit_consumers
             SET rerouted_count = rerouted_count + 1
             WHERE queue_key = ?1 AND consumer_id = ?2",
            params![queue_key, consumer_id],
        )
        .map_err(sql_error)?;
    }
    let (served, queued, rerouted): (i64, i64, i64) = conn
        .query_row(
            "SELECT served_count, queued_count, rerouted_count
             FROM durable_rate_limit_consumers
             WHERE queue_key = ?1 AND consumer_id = ?2",
            params![queue_key, consumer_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(sql_error)?;
    Ok(FairRateLimitCounters {
        served: served.max(0) as u64,
        queued: queued.max(0) as u64,
        rerouted: rerouted.max(0) as u64,
    })
}

fn prune_bucket(
    tx: &rusqlite::Transaction<'_>,
    bucket: &RateBucket,
    now_ms: i64,
) -> Result<(), ReserveOnceError> {
    let cutoff_ms = now_ms.saturating_sub(u64_to_i64(bucket.window_ms));
    tx.execute(
        "DELETE FROM durable_rate_limit_entries WHERE bucket_key = ?1 AND ts_ms <= ?2",
        params![&bucket.key, cutoff_ms],
    )
    .map_err(ReserveOnceError::Sqlite)?;
    Ok(())
}

fn bucket_wait_ms(
    tx: &rusqlite::Transaction<'_>,
    bucket: &RateBucket,
    now_ms: i64,
) -> Result<Option<u64>, ReserveOnceError> {
    let usage: i64 = tx.query_row(
        "SELECT COALESCE(SUM(units), 0)
             FROM durable_rate_limit_entries
             WHERE bucket_key = ?1",
        params![&bucket.key],
        |row| row.get(0),
    )?;
    let usage = usage.max(0) as u64;
    if usage.saturating_add(bucket.charged_units) <= bucket.limit {
        return Ok(None);
    }

    let needed = usage
        .saturating_add(bucket.charged_units)
        .saturating_sub(bucket.limit);
    let mut stmt = tx.prepare(
        "SELECT ts_ms, units
             FROM durable_rate_limit_entries
             WHERE bucket_key = ?1
             ORDER BY ts_ms ASC",
    )?;
    let mut rows = stmt.query(params![&bucket.key])?;
    let mut freed = 0_u64;
    while let Some(row) = rows.next()? {
        let ts_ms: i64 = row.get(0)?;
        let units: i64 = row.get(1)?;
        freed = freed.saturating_add(units.max(0) as u64);
        if freed >= needed {
            let expiry_ms = ts_ms.saturating_add(u64_to_i64(bucket.window_ms));
            return Ok(Some(expiry_ms.saturating_sub(now_ms).max(1) as u64));
        }
    }
    Ok(Some(bucket.window_ms.max(1)))
}

fn sql_error(error: rusqlite::Error) -> VmError {
    VmError::Runtime(format!("durable_rate_limit_acquire: sqlite error: {error}"))
}

fn u64_to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn now_wall_ms() -> i64 {
    crate::stdlib::clock::now_wall_ms().max(0)
}

async fn sleep_ms(ms: u64) {
    if ms == 0 {
        return;
    }
    if crate::stdlib::clock::is_mocked() {
        crate::stdlib::clock::advance(u64_to_i64(ms));
    } else {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}

fn result_value(
    ok: bool,
    timed_out: bool,
    waited_ms: u64,
    retry_after_ms: u64,
    state_path: &Path,
    buckets: &[RateBucket],
) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("ok".to_string(), VmValue::Bool(ok));
    dict.insert("timed_out".to_string(), VmValue::Bool(timed_out));
    dict.insert("waited_ms".to_string(), VmValue::Int(u64_to_i64(waited_ms)));
    dict.insert(
        "retry_after_ms".to_string(),
        VmValue::Int(u64_to_i64(retry_after_ms)),
    );
    dict.put_str("state_path", state_path.to_string_lossy());
    dict.insert("buckets".to_string(), bucket_list_value(buckets));
    VmValue::dict(dict)
}

fn bucket_list_value(buckets: &[RateBucket]) -> VmValue {
    VmValue::List(Arc::new(
        buckets
            .iter()
            .map(|bucket| {
                VmValue::dict(BTreeMap::from([
                    (
                        "key".to_string(),
                        VmValue::String(arcstr::ArcStr::from(bucket.key.as_str())),
                    ),
                    ("limit".to_string(), VmValue::Int(u64_to_i64(bucket.limit))),
                    ("units".to_string(), VmValue::Int(u64_to_i64(bucket.units))),
                    (
                        "charged_units".to_string(),
                        VmValue::Int(u64_to_i64(bucket.charged_units)),
                    ),
                    (
                        "window_ms".to_string(),
                        VmValue::Int(u64_to_i64(bucket.window_ms)),
                    ),
                ]))
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests;

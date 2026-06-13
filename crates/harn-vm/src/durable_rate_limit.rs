use crate::value::VmDictExt;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{params, Connection, TransactionBehavior};

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::stdlib::options::{non_negative_millis_from_value, ErrorKind};
use crate::stdlib::sandbox::{self, FsAccess};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const DEFAULT_WINDOW_MS: u64 = 60_000;
const DEFAULT_BUSY_TIMEOUT_MS: u64 = 5_000;
const MAX_SLEEP_MS: u64 = 60_000;

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

fn parse_state_path(options: &BTreeMap<String, VmValue>) -> Result<PathBuf, VmError> {
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

fn parse_buckets(options: &BTreeMap<String, VmValue>) -> Result<Vec<RateBucket>, VmError> {
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

fn parse_bucket(dict: &BTreeMap<String, VmValue>) -> Result<RateBucket, VmError> {
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
    dict: &BTreeMap<String, VmValue>,
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
    dict: &BTreeMap<String, VmValue>,
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
    dict: &BTreeMap<String, VmValue>,
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
    dict: &BTreeMap<String, VmValue>,
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

    let mut conn = Connection::open(path).map_err(sql_error)?;
    conn.busy_timeout(Duration::from_millis(DEFAULT_BUSY_TIMEOUT_MS))
        .map_err(sql_error)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS durable_rate_limit_entries (
            bucket_key TEXT NOT NULL,
            ts_ms INTEGER NOT NULL,
            units INTEGER NOT NULL CHECK(units >= 0)
         );
         CREATE INDEX IF NOT EXISTS durable_rate_limit_entries_key_ts_idx
            ON durable_rate_limit_entries(bucket_key, ts_ms);",
    )
    .map_err(sql_error)?;

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql_error)?;
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
        tx.commit().map_err(sql_error)?;
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
        .map_err(sql_error)?;
    }
    tx.commit().map_err(sql_error)?;
    Ok(ReserveAttempt {
        acquired: true,
        retry_after_ms: 0,
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
            return Err(VmError::Thrown(VmValue::String(Arc::from(
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

fn prune_bucket(
    tx: &rusqlite::Transaction<'_>,
    bucket: &RateBucket,
    now_ms: i64,
) -> Result<(), VmError> {
    let cutoff_ms = now_ms.saturating_sub(u64_to_i64(bucket.window_ms));
    tx.execute(
        "DELETE FROM durable_rate_limit_entries WHERE bucket_key = ?1 AND ts_ms <= ?2",
        params![&bucket.key, cutoff_ms],
    )
    .map_err(sql_error)?;
    Ok(())
}

fn bucket_wait_ms(
    tx: &rusqlite::Transaction<'_>,
    bucket: &RateBucket,
    now_ms: i64,
) -> Result<Option<u64>, VmError> {
    let usage: i64 = tx
        .query_row(
            "SELECT COALESCE(SUM(units), 0)
             FROM durable_rate_limit_entries
             WHERE bucket_key = ?1",
            params![&bucket.key],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let usage = usage.max(0) as u64;
    if usage.saturating_add(bucket.charged_units) <= bucket.limit {
        return Ok(None);
    }

    let needed = usage
        .saturating_add(bucket.charged_units)
        .saturating_sub(bucket.limit);
    let mut stmt = tx
        .prepare(
            "SELECT ts_ms, units
             FROM durable_rate_limit_entries
             WHERE bucket_key = ?1
             ORDER BY ts_ms ASC",
        )
        .map_err(sql_error)?;
    let mut rows = stmt.query(params![&bucket.key]).map_err(sql_error)?;
    let mut freed = 0_u64;
    while let Some(row) = rows.next().map_err(sql_error)? {
        let ts_ms: i64 = row.get(0).map_err(sql_error)?;
        let units: i64 = row.get(1).map_err(sql_error)?;
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
    VmValue::Dict(Arc::new(dict))
}

fn bucket_list_value(buckets: &[RateBucket]) -> VmValue {
    VmValue::List(Arc::new(
        buckets
            .iter()
            .map(|bucket| {
                VmValue::Dict(Arc::new(BTreeMap::from([
                    (
                        "key".to_string(),
                        VmValue::String(Arc::from(bucket.key.as_str())),
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
                ])))
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests;

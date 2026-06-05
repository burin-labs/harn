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
struct RateBucket {
    key: String,
    limit: u64,
    units: u64,
    charged_units: u64,
    window_ms: u64,
}

#[derive(Debug, PartialEq, Eq)]
struct ReserveAttempt {
    acquired: bool,
    retry_after_ms: u64,
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

    let started_ms = now_wall_ms();
    let mut waited_ms = 0_u64;
    loop {
        if vm
            .cancel_token
            .as_ref()
            .is_some_and(|token| token.load(std::sync::atomic::Ordering::SeqCst))
        {
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
            return Ok(result_value(
                true,
                false,
                waited_ms,
                0,
                &state_path,
                &buckets,
            ));
        }

        let retry_after_ms = attempt.retry_after_ms.max(1);
        let elapsed_ms = now_ms.saturating_sub(started_ms).max(0) as u64;
        if let Some(timeout_ms) = timeout_ms {
            if elapsed_ms >= timeout_ms {
                return Ok(result_value(
                    false,
                    true,
                    waited_ms,
                    retry_after_ms,
                    &state_path,
                    &buckets,
                ));
            }
            let remaining_ms = timeout_ms.saturating_sub(elapsed_ms);
            if retry_after_ms > remaining_ms {
                if remaining_ms > 0 {
                    sleep_ms(remaining_ms).await;
                    waited_ms = waited_ms.saturating_add(remaining_ms);
                }
                return Ok(result_value(
                    false,
                    true,
                    waited_ms,
                    retry_after_ms,
                    &state_path,
                    &buckets,
                ));
            }
        }

        let sleep_for_ms = retry_after_ms.min(MAX_SLEEP_MS);
        sleep_ms(sleep_for_ms).await;
        waited_ms = waited_ms.saturating_add(sleep_for_ms);
    }
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
    let charged_units = if units == 0 { 0 } else { units.min(limit) };
    Ok(RateBucket {
        key,
        limit,
        units,
        charged_units,
        window_ms,
    })
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
    dict.insert(
        "state_path".to_string(),
        VmValue::String(Arc::from(state_path.to_string_lossy().into_owned())),
    );
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
mod tests {
    use super::*;
    use crate::{compile_source, register_vm_stdlib, reset_thread_local_state, Vm};
    use std::sync::Barrier;

    async fn run_harn(base_dir: &std::path::Path, source: &str) -> Vec<String> {
        reset_thread_local_state();
        let chunk = compile_source(source).expect("compile source");
        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);
        vm.set_source_dir(base_dir);
        vm.execute(&chunk).await.expect("execute source");
        vm.output()
            .trim_end()
            .lines()
            .map(ToString::to_string)
            .collect()
    }

    fn bucket(key: &str, limit: u64, units: u64, window_ms: u64) -> RateBucket {
        RateBucket {
            key: key.to_string(),
            limit,
            units,
            charged_units: units.min(limit),
            window_ms,
        }
    }

    fn usage(path: &Path, key: &str) -> u64 {
        let conn = Connection::open(path).expect("open sqlite");
        conn.query_row(
            "SELECT COALESCE(SUM(units), 0)
             FROM durable_rate_limit_entries
             WHERE bucket_key = ?1",
            params![key],
            |row| row.get::<_, i64>(0),
        )
        .expect("query")
        .max(0) as u64
    }

    #[test]
    fn reserve_blocks_until_window_expires() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("rate.sqlite");
        let buckets = vec![bucket("provider:rpm", 1, 1, 1_000)];

        assert!(
            try_reserve_once(&path, &buckets, 10_000)
                .expect("first reserve")
                .acquired
        );
        let blocked = try_reserve_once(&path, &buckets, 10_250).expect("blocked reserve");
        assert_eq!(
            blocked,
            ReserveAttempt {
                acquired: false,
                retry_after_ms: 750
            }
        );
        assert!(
            try_reserve_once(&path, &buckets, 11_000)
                .expect("expired reserve")
                .acquired
        );
    }

    #[test]
    fn multi_bucket_reservation_is_atomic_when_one_bucket_is_full() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("rate.sqlite");
        let first = vec![
            bucket("provider:rpm", 1, 1, 1_000),
            bucket("model:tpm", 100, 50, 1_000),
        ];
        assert!(
            try_reserve_once(&path, &first, 1_000)
                .expect("initial reserve")
                .acquired
        );

        let second = vec![
            bucket("provider:rpm", 1, 1, 1_000),
            bucket("model:tpm", 100, 10, 1_000),
        ];
        let blocked = try_reserve_once(&path, &second, 1_100).expect("blocked reserve");
        assert!(!blocked.acquired);
        assert_eq!(usage(&path, "model:tpm"), 50);
    }

    #[test]
    fn oversized_reservation_charges_one_full_window() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("rate.sqlite");
        let buckets = vec![bucket("model:tpm", 100, 250, 1_000)];

        assert!(
            try_reserve_once(&path, &buckets, 1_000)
                .expect("oversized reserve")
                .acquired
        );
        assert_eq!(usage(&path, "model:tpm"), 100);
    }

    #[test]
    fn concurrent_threads_do_not_over_reserve_shared_bucket() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("rate.sqlite");
        let buckets = vec![bucket("provider:rpm", 1, 1, 60_000)];
        let barrier = Arc::new(Barrier::new(8));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let path = path.clone();
            let buckets = buckets.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                try_reserve_once(&path, &buckets, 1_000).expect("reserve")
            }));
        }

        let attempts: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .collect();
        assert_eq!(
            attempts.iter().filter(|attempt| attempt.acquired).count(),
            1
        );
        assert_eq!(
            attempts.iter().filter(|attempt| !attempt.acquired).count(),
            7
        );
        assert_eq!(usage(&path, "provider:rpm"), 1);
    }

    #[test]
    fn duplicate_bucket_keys_are_rejected() {
        let options = BTreeMap::from([(
            "buckets".to_string(),
            VmValue::List(Arc::new(vec![
                VmValue::Dict(Arc::new(BTreeMap::from([
                    ("key".to_string(), VmValue::String(Arc::from("same"))),
                    ("limit".to_string(), VmValue::Int(1)),
                ]))),
                VmValue::Dict(Arc::new(BTreeMap::from([
                    ("key".to_string(), VmValue::String(Arc::from("same"))),
                    ("limit".to_string(), VmValue::Int(1)),
                ]))),
            ])),
        )]);
        let error = parse_buckets(&options).expect_err("duplicate keys should fail");
        assert!(error.to_string().contains("duplicate bucket key `same`"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn harn_builtin_returns_structured_timeout_without_real_sleep() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let temp = tempfile::tempdir().expect("tempdir");
                let state_path = harn_string_path(temp.path().join("rate.sqlite"));
                let source = r#"
pipeline main(task) {
  mock_time(1000)
  let first = durable_rate_limit_acquire({
    state_path: "__STATE_PATH__",
    key: "provider:rpm",
    limit: 1,
    units: 1,
    window_ms: 1000,
  })
  let second = durable_rate_limit_acquire({
    state_path: "__STATE_PATH__",
    key: "provider:rpm",
    limit: 1,
    units: 1,
    window_ms: 1000,
    timeout_ms: 0,
  })
  __io_println(to_string(first.ok))
  __io_println(to_string(second.ok))
  __io_println(to_string(second.timed_out))
  __io_println(to_string(second.retry_after_ms))
}
"#
                .replace("__STATE_PATH__", &state_path);
                let lines = run_harn(temp.path(), &source).await;
                assert_eq!(lines, vec!["true", "false", "true", "1000"]);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn harn_parallel_tasks_share_one_durable_bucket() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let temp = tempfile::tempdir().expect("tempdir");
                let state_path = harn_string_path(temp.path().join("rate.sqlite"));
                let source = r#"
pipeline main(task) {
  mock_time(1000)
  let attempts = parallel each [1, 2, 3, 4] with { max_concurrent: 4 } { _ ->
    durable_rate_limit_acquire({
      state_path: "__STATE_PATH__",
      key: "provider:rpm",
      limit: 1,
      units: 1,
      window_ms: 60000,
      timeout_ms: 0,
    })
  }
  var successes = 0
  var timeouts = 0
  for attempt in attempts {
    if attempt.ok {
      successes = successes + 1
    }
    if attempt.timed_out {
      timeouts = timeouts + 1
    }
  }
  __io_println(to_string(successes))
  __io_println(to_string(timeouts))
}
"#
                .replace("__STATE_PATH__", &state_path);
                let lines = run_harn(temp.path(), &source).await;
                assert_eq!(lines, vec!["1", "3"]);
            })
            .await;
    }

    #[test]
    fn harn_vms_on_multiple_threads_share_one_durable_bucket() {
        let temp = tempfile::tempdir().expect("tempdir");
        let base_dir = temp.path().to_path_buf();
        let state_path = harn_string_path(temp.path().join("rate.sqlite"));
        let source = Arc::new(
            r#"
pipeline main(task) {
  let attempt = durable_rate_limit_acquire({
    state_path: "__STATE_PATH__",
    key: "provider:rpm",
    limit: 1,
    units: 1,
    window_ms: 60000,
    timeout_ms: 0,
  })
  __io_println(to_string(attempt.ok))
  __io_println(to_string(attempt.timed_out))
}
"#
            .replace("__STATE_PATH__", &state_path),
        );
        let barrier = Arc::new(Barrier::new(4));

        let mut handles = Vec::new();
        for _ in 0..4 {
            let base_dir = base_dir.clone();
            let source = source.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .expect("current-thread runtime");
                barrier.wait();
                runtime.block_on(
                    tokio::task::LocalSet::new()
                        .run_until(async { run_harn(&base_dir, &source).await }),
                )
            }));
        }

        let outputs: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .collect();
        assert_eq!(
            outputs
                .iter()
                .filter(|lines| lines.as_slice() == ["true", "false"])
                .count(),
            1
        );
        assert_eq!(
            outputs
                .iter()
                .filter(|lines| lines.as_slice() == ["false", "true"])
                .count(),
            3
        );
    }

    fn harn_string_path(path: PathBuf) -> String {
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    }
}

use super::*;
use crate::{compile_source, register_vm_stdlib, reset_thread_local_state, Vm};
use rusqlite::{params, Connection, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Barrier};
use std::time::Duration;

const TRANSIENT_LOCK_TEST_BUSY_TIMEOUT_MS: u64 = 25;
const TRANSIENT_LOCK_SIGNAL_TIMEOUT: Duration = Duration::from_secs(2);

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
    RateBucket::new(key.to_string(), limit, units, window_ms)
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

// Multi-session hardening: the durable rate-limit DB must open in WAL mode so
// several sessions sharing one project's limiter serialize on the write lock
// (bounded by busy_timeout) instead of throwing "database is locked" into the
// agent loop.
#[test]
fn rate_limit_db_uses_wal_journal_mode() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("rate.sqlite");
    let buckets = vec![bucket("provider:rpm", 10, 1, 1_000)];
    // Create the DB (and apply its pragmas) via the production reserve path.
    try_reserve_once(&path, &buckets, 1_000).expect("reserve");

    let conn = Connection::open(&path).expect("open sqlite");
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("read journal_mode");
    assert_eq!(
        mode.to_lowercase(),
        "wal",
        "rate-limit sqlite must be WAL for cross-session concurrency"
    );
}

#[test]
fn fair_queue_migrates_the_existing_quota_ledger_in_place() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("rate.sqlite");
    let conn = Connection::open(&path).expect("open sqlite");
    let legacy = RuntimeSqliteSchema::new(
        "durable_rate_limit",
        1,
        "CREATE TABLE durable_rate_limit_entries (
            bucket_key TEXT NOT NULL,
            ts_ms INTEGER NOT NULL,
            units INTEGER NOT NULL CHECK(units >= 0)
        );
        CREATE INDEX durable_rate_limit_entries_key_ts_idx
            ON durable_rate_limit_entries(bucket_key, ts_ms);",
    );
    initialize_runtime_sqlite(&conn, Duration::from_secs(5), &legacy)
        .expect("initialize version-one ledger");
    conn.execute(
        "INSERT INTO durable_rate_limit_entries (bucket_key, ts_ms, units)
         VALUES ('provider:rpm', 1000, 1)",
        [],
    )
    .expect("seed legacy usage");
    drop(conn);

    let attempt = try_reserve_fair_once(
        &path,
        &[bucket("provider:rpm", 2, 1, 60_000)],
        "provider",
        "consumer",
        None,
        1_001,
        60_000,
    )
    .expect("migrate and reserve");
    assert!(attempt.acquired);
    assert_eq!(usage(&path, "provider:rpm"), 2);

    let conn = Connection::open(path).expect("reopen migrated ledger");
    let version: i64 = conn
        .query_row(
            "SELECT version FROM _harn_sqlite_schema_versions
             WHERE name = 'durable_rate_limit'",
            [],
            |row| row.get(0),
        )
        .expect("read migrated marker");
    assert_eq!(version, 2);
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
fn fair_queue_serves_a_cold_consumer_before_a_hot_consumer_repeats() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("rate.sqlite");
    let buckets = vec![bucket("provider:rpm", 1, 1, 1_000)];

    let first_a = try_reserve_fair_once(&path, &buckets, "provider", "a", None, 1_000, 60_000)
        .expect("first consumer acquires");
    assert!(first_a.acquired);
    assert_eq!(first_a.counters.served, 1);

    let queued_a = try_reserve_fair_once(&path, &buckets, "provider", "a", None, 1_001, 60_000)
        .expect("hot consumer queues");
    assert!(!queued_a.acquired);
    assert_eq!(queued_a.snapshot.as_ref().unwrap().queue_position, 1);

    let queued_b = try_reserve_fair_once(&path, &buckets, "provider", "b", None, 1_002, 60_000)
        .expect("cold consumer queues");
    assert!(!queued_b.acquired);

    let still_queued_a = try_reserve_fair_once(
        &path,
        &buckets,
        "provider",
        "a",
        queued_a.ticket_id,
        2_000,
        60_000,
    )
    .expect("hot consumer remains queued");
    assert!(!still_queued_a.acquired);
    assert_eq!(
        still_queued_a.snapshot.as_ref().unwrap().queue_position,
        2,
        "least-recently-served consumer must move ahead of a repeat caller"
    );

    let served_b = try_reserve_fair_once(
        &path,
        &buckets,
        "provider",
        "b",
        queued_b.ticket_id,
        2_000,
        60_000,
    )
    .expect("cold consumer acquires");
    assert!(served_b.acquired);
    assert_eq!(served_b.counters.served, 1);
    assert_eq!(served_b.counters.queued, 1);
    assert_eq!(served_b.counters.rerouted, 0);
}

#[test]
fn fair_queue_promotes_a_starving_consumer_ahead_of_new_arrivals() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("rate.sqlite");
    let buckets = vec![bucket("provider:rpm", 1, 1, 1_000_000)];

    assert!(
        try_reserve_fair_once(&path, &buckets, "provider", "a", None, 1_000, 60_000)
            .expect("seed quota")
            .acquired
    );
    let starving = try_reserve_fair_once(&path, &buckets, "provider", "a", None, 1_001, 60_000)
        .expect("first consumer queues");
    let newer = try_reserve_fair_once(&path, &buckets, "provider", "b", None, 61_002, 60_000)
        .expect("new consumer queues");
    assert_eq!(newer.snapshot.as_ref().unwrap().queue_position, 2);

    let promoted = try_reserve_fair_once(
        &path,
        &buckets,
        "provider",
        "a",
        starving.ticket_id,
        61_002,
        60_000,
    )
    .expect("starving consumer is reconsidered");
    assert_eq!(promoted.snapshot.as_ref().unwrap().queue_position, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn fair_queue_reports_backpressure_and_counts_reroute_on_timeout() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("rate.sqlite");
    let buckets = vec![bucket("provider:rpm", 1, 1, 60_000)];
    let _clock = crate::stdlib::clock::MockClockGuard::install(1_000);

    let first = acquire_fair_durable_rate_limit(
        path.clone(),
        buckets.clone(),
        "provider".to_string(),
        "consumer-a".to_string(),
        None,
        60_000,
        false,
        || false,
        |_| {},
    )
    .await
    .expect("first consumer acquires");
    assert!(first.acquired);

    let mut snapshots = Vec::new();
    let second = acquire_fair_durable_rate_limit(
        path,
        buckets,
        "provider".to_string(),
        "consumer-b".to_string(),
        Some(500),
        60_000,
        true,
        || false,
        |snapshot| snapshots.push(snapshot.clone()),
    )
    .await
    .expect("second consumer times out structurally");

    assert!(!second.acquired);
    assert!(second.timed_out);
    assert_eq!(second.waited_ms, 500);
    assert_eq!(second.counters.queued, 1);
    assert_eq!(second.counters.rerouted, 1);
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].queue_position, 1);
    assert_eq!(snapshots[0].expected_wait_ms, 60_000);
}

#[test]
fn transient_sqlite_write_lock_retries_instead_of_erroring() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("rate.sqlite");
    let setup = vec![bucket("setup", 1, 0, 60_000)];
    try_reserve_once(&path, &setup, 1_000).expect("create schema");

    let mut locker = Connection::open(&path).expect("open sqlite");
    locker
        .busy_timeout(Duration::from_millis(DEFAULT_BUSY_TIMEOUT_MS))
        .expect("busy timeout");
    let tx = locker
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("hold write lock");

    let path_for_thread = path.clone();
    let buckets = vec![bucket("provider:rpm", 1, 1, 60_000)];
    let (busy_tx, busy_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        try_reserve_once_with_options(
            &path_for_thread,
            &buckets,
            1_000,
            TRANSIENT_LOCK_TEST_BUSY_TIMEOUT_MS,
            DEFAULT_BUSY_TIMEOUT_MS,
            move || {
                let _ = busy_tx.send(());
            },
        )
    });

    busy_rx
        .recv_timeout(TRANSIENT_LOCK_SIGNAL_TIMEOUT)
        .expect("worker should observe the transient sqlite lock before retrying");
    drop(tx);

    let attempt = handle
        .join()
        .expect("thread")
        .expect("reserve should retry the transient sqlite lock");
    assert!(
        attempt.acquired,
        "reservation succeeds once the write lock clears"
    );
    assert_eq!(usage(&path, "provider:rpm"), 1);
}

#[test]
fn duplicate_bucket_keys_are_rejected() {
    let options = crate::value::DictMap::from_iter([(
        "buckets".to_string(),
        VmValue::List(Arc::new(vec![
            VmValue::dict(crate::value::DictMap::from_iter([
                (
                    "key".to_string(),
                    VmValue::String(arcstr::ArcStr::from("same")),
                ),
                ("limit".to_string(), VmValue::Int(1)),
            ])),
            VmValue::dict(crate::value::DictMap::from_iter([
                (
                    "key".to_string(),
                    VmValue::String(arcstr::ArcStr::from("same")),
                ),
                ("limit".to_string(), VmValue::Int(1)),
            ])),
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
  const first = durable_rate_limit_acquire({
    state_path: "__STATE_PATH__",
    key: "provider:rpm",
    limit: 1,
    units: 1,
    window_ms: 1000,
  })
  const second = durable_rate_limit_acquire({
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
  const attempts = parallel each [1, 2, 3, 4] with { max_concurrent: 4 } { _ ->
    durable_rate_limit_acquire({
      state_path: "__STATE_PATH__",
      key: "provider:rpm",
      limit: 1,
      units: 1,
      window_ms: 60000,
      timeout_ms: 0,
    })
  }
  let successes = 0
  let timeouts = 0
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
  const attempt = durable_rate_limit_acquire({
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

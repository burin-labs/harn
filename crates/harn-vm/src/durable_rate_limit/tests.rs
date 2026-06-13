use super::*;
use crate::{compile_source, register_vm_stdlib, reset_thread_local_state, Vm};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

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
    let options = crate::value::DictMap::from_iter([(
        "buckets".to_string(),
        VmValue::List(Arc::new(vec![
            VmValue::dict(crate::value::DictMap::from_iter([
                ("key".to_string(), VmValue::String(Arc::from("same"))),
                ("limit".to_string(), VmValue::Int(1)),
            ])),
            VmValue::dict(crate::value::DictMap::from_iter([
                ("key".to_string(), VmValue::String(Arc::from("same"))),
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

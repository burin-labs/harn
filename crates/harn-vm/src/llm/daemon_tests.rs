use super::*;
use crate::value::VmValue;
use std::collections::BTreeMap;
use std::rc::Rc;

#[test]
fn daemon_snapshot_roundtrip_preserves_state() {
    let dir = std::env::temp_dir().join(format!("harn-daemon-{}", uuid::Uuid::now_v7()));
    let path = dir.join("daemon.json");
    let snapshot = DaemonSnapshot {
        daemon_state: "idle".to_string(),
        visible_messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
        total_iterations: 2,
        idle_backoff_ms: 500,
        ..Default::default()
    };
    persist_snapshot(path.to_str().unwrap(), &snapshot).unwrap();
    let loaded = load_snapshot(path.to_str().unwrap()).unwrap();
    assert_eq!(loaded.daemon_state, "idle");
    assert_eq!(loaded.visible_messages.len(), 1);
    assert_eq!(loaded.total_iterations, 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn detect_watch_changes_reports_modified_files() {
    let mock = MockMtimeProvider::new();
    let path = "/virtual/watched.txt".to_string();
    let paths = vec![path.clone()];

    mock.set(&path, 1_000_000);
    let mut state = watch_state(&mock, &paths);
    assert_eq!(state.get(&path).copied(), Some(1_000_000));

    let unchanged = detect_watch_changes(&mock, &paths, &mut state);
    assert!(
        unchanged.is_empty(),
        "unchanged mtime must not be reported as modified"
    );

    mock.advance(&path, 100);
    let changed = detect_watch_changes(&mock, &paths, &mut state);
    assert_eq!(changed, paths, "advanced mtime must be reported");
    assert_eq!(state.get(&path).copied(), Some(1_000_100));

    let stable = detect_watch_changes(&mock, &paths, &mut state);
    assert!(
        stable.is_empty(),
        "post-detection state should match current mtime, blocking re-trigger"
    );
}

#[test]
fn detect_watch_changes_isolates_changed_paths() {
    let mock = MockMtimeProvider::new();
    let a = "/virtual/a.txt".to_string();
    let b = "/virtual/b.txt".to_string();
    let c = "/virtual/c.txt".to_string();
    let paths = vec![a.clone(), b.clone(), c.clone()];

    mock.set(&a, 100);
    mock.set(&b, 200);
    mock.set(&c, 300);
    let mut state = watch_state(&mock, &paths);

    mock.advance(&b, 1);
    let changed = detect_watch_changes(&mock, &paths, &mut state);
    assert_eq!(changed, vec![b.clone()]);

    mock.advance(&a, 5);
    mock.advance(&c, 7);
    let changed = detect_watch_changes(&mock, &paths, &mut state);
    assert_eq!(changed, vec![a.clone(), c.clone()]);
}

#[test]
fn detect_watch_changes_ignores_missing_paths() {
    // `file_stamp` returns 0 for missing files. The detector treats 0
    // (either prior or current) as "don't know", so a path appearing
    // or disappearing must not register as a change. This protects
    // against false wakes when watch_paths reference files that don't
    // exist yet.
    let mock = MockMtimeProvider::new();
    let path = "/virtual/maybe.txt".to_string();
    let paths = vec![path.clone()];

    let mut state = watch_state(&mock, &paths);
    assert_eq!(state.get(&path).copied(), Some(0));

    mock.set(&path, 500);
    let appearing = detect_watch_changes(&mock, &paths, &mut state);
    assert!(
        appearing.is_empty(),
        "missing→present transition is not a change"
    );
    assert_eq!(state.get(&path).copied(), Some(500));

    mock.clear(&path);
    let disappearing = detect_watch_changes(&mock, &paths, &mut state);
    assert!(
        disappearing.is_empty(),
        "present→missing transition is not a change"
    );
    assert_eq!(state.get(&path).copied(), Some(0));
}

#[test]
fn real_mtime_provider_reads_filesystem_mtime() {
    // Pin RealMtimeProvider behavior: existing files yield non-zero,
    // missing files yield zero. The watcher's logic depends on this
    // contract, and the mock would mask a regression here.
    let dir = std::env::temp_dir().join(format!("harn-mtime-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("present.txt");
    std::fs::write(&path, "x").unwrap();

    let real = RealMtimeProvider;
    assert!(real.mtime_ns(path.to_str().unwrap()) > 0);
    let missing = dir.join("absent.txt");
    assert_eq!(real.mtime_ns(missing.to_str().unwrap()), 0);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn parse_daemon_config_reads_top_level_options() {
    let mut options = BTreeMap::new();
    options.insert(
        "persist_path".to_string(),
        VmValue::String(Rc::from("/tmp/daemon.json")),
    );
    options.insert(
        "resume_path".to_string(),
        VmValue::String(Rc::from("/tmp/daemon-resume.json")),
    );
    options.insert("wake_interval_ms".to_string(), VmValue::Int(250));
    options.insert(
        "watch_paths".to_string(),
        VmValue::List(Rc::new(vec![
            VmValue::String(Rc::from("a.txt")),
            VmValue::String(Rc::from("b.txt")),
        ])),
    );
    options.insert("consolidate_on_idle".to_string(), VmValue::Bool(true));

    let config = parse_daemon_loop_config(Some(&options));
    assert_eq!(config.persist_path.as_deref(), Some("/tmp/daemon.json"));
    assert_eq!(
        config.resume_path.as_deref(),
        Some("/tmp/daemon-resume.json")
    );
    assert_eq!(config.wake_interval_ms, Some(250));
    assert_eq!(config.watch_paths, vec!["a.txt", "b.txt"]);
    assert!(config.consolidate_on_idle);
}

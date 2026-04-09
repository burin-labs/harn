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
    let dir = std::env::temp_dir().join(format!("harn-watch-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("watched.txt");
    std::fs::write(&path, "one").unwrap();
    let paths = vec![path.to_string_lossy().to_string()];
    let mut state = watch_state(&paths);
    std::thread::sleep(std::time::Duration::from_secs(1));
    std::fs::write(&path, "two").unwrap();
    let changed = detect_watch_changes(&paths, &mut state);
    assert_eq!(changed, paths);
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

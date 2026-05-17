//! Integration tests for the per-tool-call FS snapshot primitives.
//!
//! Exercises both the explicit `hostlib_fs_snapshot({paths: [...]})` form
//! and the auto-on-write path that snaps pre-images out of
//! `tools/write_file` / `tools/delete_file` when an open snapshot is
//! registered for the current tool call.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use harn_hostlib::tools::permissions;
use harn_hostlib::{
    fs_snapshot::FsSnapshotCapability, tools::ToolsCapability, BuiltinRegistry, HostlibCapability,
};
use harn_vm::agent_sessions;
use harn_vm::VmValue;
use tempfile::TempDir;

/// Serialize tests in this binary. Each test mutates process-wide
/// snapshot state and thread-local session/tool-call stacks; parallel
/// execution observed `reset_for_test` racing the auto-capture path.
fn test_guard() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn registry() -> BuiltinRegistry {
    permissions::reset();
    permissions::enable_for_test();
    let mut registry = BuiltinRegistry::new();
    FsSnapshotCapability.register_builtins(&mut registry);
    ToolsCapability.register_builtins(&mut registry);
    registry
}

fn dict_arg(entries: &[(&str, VmValue)]) -> Vec<VmValue> {
    let mut map: BTreeMap<String, VmValue> = BTreeMap::new();
    for (k, v) in entries {
        map.insert(k.to_string(), v.clone());
    }
    vec![VmValue::Dict(Rc::new(map))]
}

fn vm_string(s: &str) -> VmValue {
    VmValue::String(Rc::from(s))
}

fn dict_get<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
    match value {
        VmValue::Dict(d) => d.get(key).expect("key present"),
        other => panic!("not a dict: {other:?}"),
    }
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn unique(prefix: &str) -> String {
    static COUNTER: AtomicUsize = AtomicUsize::new(1);
    format!("{prefix}-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[test]
fn explicit_snapshot_then_restore_through_builtins() {
    let _guard = test_guard();
    harn_hostlib::fs_snapshot::reset_for_test();
    agent_sessions::reset_session_store();
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("explicit.txt");
    fs::write(&file, b"original").unwrap();
    let session = unique("snap-explicit");
    agent_sessions::open_or_create(Some(session.clone()));
    let _session_guard = agent_sessions::enter_current_session(session.clone());
    let reg = registry();

    let snapshot = (reg.find("hostlib_fs_snapshot").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("scope_id", vm_string("tc-explicit")),
        (
            "paths",
            VmValue::List(Rc::new(vec![vm_string(&path_str(&file))])),
        ),
        ("root", vm_string(&path_str(dir.path()))),
    ]))
    .unwrap();
    assert!(
        matches!(dict_get(&snapshot, "snapshot_id"), VmValue::String(id) if id.as_ref() == "tc-explicit")
    );
    assert!(matches!(dict_get(&snapshot, "byte_count"), VmValue::Int(8)));

    fs::write(&file, b"corrupted").unwrap();

    let restored = (reg.find("hostlib_fs_restore").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("snapshot_id", vm_string("tc-explicit")),
    ]))
    .unwrap();
    let restored_paths = match dict_get(&restored, "restored_paths") {
        VmValue::List(items) => items.iter().count(),
        other => panic!("restored_paths not a list: {other:?}"),
    };
    assert_eq!(restored_paths, 1);
    assert_eq!(fs::read(&file).unwrap(), b"original");
}

#[test]
fn auto_on_write_captures_pre_image_via_tools_write_file() {
    let _guard = test_guard();
    harn_hostlib::fs_snapshot::reset_for_test();
    agent_sessions::reset_session_store();
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("auto.txt");
    fs::write(&file, b"pre").unwrap();
    let session = unique("snap-auto");
    agent_sessions::open_or_create(Some(session.clone()));
    let _session_guard = agent_sessions::enter_current_session(session.clone());
    let _tool_guard = agent_sessions::enter_current_tool_call("tc-auto");
    let reg = registry();

    // Register an open snapshot. No paths provided so capture is lazy.
    (reg.find("hostlib_fs_snapshot").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("scope_id", vm_string("tc-auto")),
        ("root", vm_string(&path_str(dir.path()))),
    ]))
    .unwrap();

    // Mutate through the tool builtin. Pre-image must be captured first.
    (reg.find("hostlib_tools_write_file").unwrap().handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string("post")),
    ]))
    .unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"post");

    // Restore the snapshot and confirm the byte-for-byte pre-image is back.
    (reg.find("hostlib_fs_restore").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("snapshot_id", vm_string("tc-auto")),
    ]))
    .unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"pre");
}

#[test]
fn auto_on_write_captures_delete_so_restore_reinstates_file() {
    let _guard = test_guard();
    harn_hostlib::fs_snapshot::reset_for_test();
    agent_sessions::reset_session_store();
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("victim.txt");
    fs::write(&file, b"important").unwrap();
    let session = unique("snap-delete");
    agent_sessions::open_or_create(Some(session.clone()));
    let _session_guard = agent_sessions::enter_current_session(session.clone());
    let _tool_guard = agent_sessions::enter_current_tool_call("tc-delete");
    let reg = registry();

    (reg.find("hostlib_fs_snapshot").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("scope_id", vm_string("tc-delete")),
        ("root", vm_string(&path_str(dir.path()))),
    ]))
    .unwrap();

    (reg.find("hostlib_tools_delete_file").unwrap().handler)(&dict_arg(&[(
        "path",
        vm_string(&path_str(&file)),
    )]))
    .unwrap();
    assert!(!file.exists());

    (reg.find("hostlib_fs_restore").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("snapshot_id", vm_string("tc-delete")),
    ]))
    .unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"important");
}

#[test]
fn list_and_drop_remove_snapshot_state_through_builtins() {
    let _guard = test_guard();
    harn_hostlib::fs_snapshot::reset_for_test();
    agent_sessions::reset_session_store();
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("listed.txt");
    fs::write(&file, b"abcd").unwrap();
    let session = unique("snap-list");
    agent_sessions::open_or_create(Some(session.clone()));
    let _session_guard = agent_sessions::enter_current_session(session.clone());
    let reg = registry();

    (reg.find("hostlib_fs_snapshot").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("scope_id", vm_string("tc-list")),
        (
            "paths",
            VmValue::List(Rc::new(vec![vm_string(&path_str(&file))])),
        ),
        ("root", vm_string(&path_str(dir.path()))),
    ]))
    .unwrap();

    let listed = (reg.find("hostlib_fs_list_snapshots").unwrap().handler)(&dict_arg(&[(
        "session_id",
        vm_string(&session),
    )]))
    .unwrap();
    let count = match dict_get(&listed, "snapshots") {
        VmValue::List(items) => items.len(),
        other => panic!("snapshots not a list: {other:?}"),
    };
    assert_eq!(count, 1);

    let dropped = (reg.find("hostlib_fs_drop_snapshot").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&session)),
        ("snapshot_id", vm_string("tc-list")),
    ]))
    .unwrap();
    assert!(matches!(dict_get(&dropped, "dropped"), VmValue::Bool(true)));
    let listed_after = (reg.find("hostlib_fs_list_snapshots").unwrap().handler)(&dict_arg(&[(
        "session_id",
        vm_string(&session),
    )]))
    .unwrap();
    assert!(matches!(
        dict_get(&listed_after, "snapshots"),
        VmValue::List(items) if items.is_empty()
    ));
}

#[test]
fn auto_on_write_no_ops_when_no_snapshot_is_registered() {
    let _guard = test_guard();
    harn_hostlib::fs_snapshot::reset_for_test();
    agent_sessions::reset_session_store();
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("plain.txt");
    let session = unique("snap-noop");
    agent_sessions::open_or_create(Some(session.clone()));
    let _session_guard = agent_sessions::enter_current_session(session.clone());
    let reg = registry();

    // No fs_snapshot call — write should be a plain mutation.
    (reg.find("hostlib_tools_write_file").unwrap().handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string("hi")),
    ]))
    .unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"hi");

    let listed = (reg.find("hostlib_fs_list_snapshots").unwrap().handler)(&dict_arg(&[(
        "session_id",
        vm_string(&session),
    )]))
    .unwrap();
    assert!(matches!(
        dict_get(&listed, "snapshots"),
        VmValue::List(items) if items.is_empty()
    ));
}

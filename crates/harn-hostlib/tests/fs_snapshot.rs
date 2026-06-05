//! Integration tests for the per-tool-call FS snapshot primitives.
//!
//! Exercises both the explicit `hostlib_fs_snapshot({paths: [...]})` form
//! and the auto-on-write path that snaps pre-images out of
//! `tools/write_file` / `tools/delete_file` for the current tool call.
//!
//! Tests rely on session-id isolation, not serialization: every test
//! mints a unique session id, so the process-wide snapshot store keys
//! cleanly. No process-wide reset is required.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use harn_hostlib::tools::permissions;
use harn_hostlib::{
    fs_snapshot::FsSnapshotCapability, tools::ToolsCapability, BuiltinRegistry, HostlibCapability,
};
use harn_vm::agent_sessions;
use harn_vm::VmValue;
use tempfile::TempDir;

fn registry() -> BuiltinRegistry {
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
    vec![VmValue::Dict(Arc::new(map))]
}

fn vm_string(s: &str) -> VmValue {
    VmValue::String(Arc::from(s))
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

fn unique_session(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{n}-{}", std::process::id())
}

fn unique_scope() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("tc-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

struct Fixture {
    session: String,
    scope: String,
    _session_guard: agent_sessions::CurrentSessionGuard,
    _tool_guard: Option<agent_sessions::CurrentToolCallGuard>,
}

impl Fixture {
    /// Set up a session + scope id and enter both onto the thread-local
    /// stacks. `enter_tool_call` toggles whether the auto-on-write path
    /// can fire; tests that exercise the explicit-paths surface only
    /// don't need it.
    fn new(prefix: &str, enter_tool_call: bool) -> Self {
        let session = unique_session(prefix);
        let scope = unique_scope();
        agent_sessions::open_or_create(Some(session.clone()));
        let session_guard = agent_sessions::enter_current_session(session.clone());
        let tool_guard = if enter_tool_call {
            Some(agent_sessions::enter_current_tool_call(scope.clone()))
        } else {
            None
        };
        Self {
            session,
            scope,
            _session_guard: session_guard,
            _tool_guard: tool_guard,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Best-effort cleanup so successive tests inside a single shared
        // process don't leak `.harn/state/snapshots/<session>/` dirs
        // under their TempDirs. Snapshot state is keyed by session id,
        // so this only ever touches this test's data.
        harn_hostlib::fs_snapshot::drop_session_snapshots(&self.session);
    }
}

#[test]
fn explicit_snapshot_then_restore_through_builtins() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("explicit.txt");
    fs::write(&file, b"original").unwrap();
    let fixture = Fixture::new("snap-explicit", false);
    let reg = registry();

    let snapshot = (reg.find("hostlib_fs_snapshot").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&fixture.session)),
        ("scope_id", vm_string(&fixture.scope)),
        (
            "paths",
            VmValue::List(Arc::new(vec![vm_string(&path_str(&file))])),
        ),
        ("root", vm_string(&path_str(dir.path()))),
    ]))
    .unwrap();
    assert!(matches!(
        dict_get(&snapshot, "snapshot_id"),
        VmValue::String(id) if id.as_ref() == fixture.scope
    ));
    assert!(matches!(dict_get(&snapshot, "byte_count"), VmValue::Int(8)));

    fs::write(&file, b"corrupted").unwrap();

    let restored = (reg.find("hostlib_fs_restore").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&fixture.session)),
        ("snapshot_id", vm_string(&fixture.scope)),
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
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("auto.txt");
    fs::write(&file, b"pre").unwrap();
    let fixture = Fixture::new("snap-auto", true);
    let reg = registry();

    // Register an open snapshot. No paths provided so capture is lazy.
    (reg.find("hostlib_fs_snapshot").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&fixture.session)),
        ("scope_id", vm_string(&fixture.scope)),
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

    // Restore and confirm the byte-for-byte pre-image is back.
    (reg.find("hostlib_fs_restore").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&fixture.session)),
        ("snapshot_id", vm_string(&fixture.scope)),
    ]))
    .unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"pre");
}

#[test]
fn auto_on_write_captures_delete_so_restore_reinstates_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("victim.txt");
    fs::write(&file, b"important").unwrap();
    let fixture = Fixture::new("snap-delete", true);
    let reg = registry();

    (reg.find("hostlib_fs_snapshot").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&fixture.session)),
        ("scope_id", vm_string(&fixture.scope)),
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
        ("session_id", vm_string(&fixture.session)),
        ("snapshot_id", vm_string(&fixture.scope)),
    ]))
    .unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"important");
}

#[test]
fn list_and_drop_remove_snapshot_state_through_builtins() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("listed.txt");
    fs::write(&file, b"abcd").unwrap();
    let fixture = Fixture::new("snap-list", false);
    let reg = registry();

    (reg.find("hostlib_fs_snapshot").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&fixture.session)),
        ("scope_id", vm_string(&fixture.scope)),
        (
            "paths",
            VmValue::List(Arc::new(vec![vm_string(&path_str(&file))])),
        ),
        ("root", vm_string(&path_str(dir.path()))),
    ]))
    .unwrap();

    let listed = (reg.find("hostlib_fs_list_snapshots").unwrap().handler)(&dict_arg(&[(
        "session_id",
        vm_string(&fixture.session),
    )]))
    .unwrap();
    let count = match dict_get(&listed, "snapshots") {
        VmValue::List(items) => items.len(),
        other => panic!("snapshots not a list: {other:?}"),
    };
    assert_eq!(count, 1);

    let dropped = (reg.find("hostlib_fs_drop_snapshot").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&fixture.session)),
        ("snapshot_id", vm_string(&fixture.scope)),
    ]))
    .unwrap();
    assert!(matches!(dict_get(&dropped, "dropped"), VmValue::Bool(true)));
    let listed_after = (reg.find("hostlib_fs_list_snapshots").unwrap().handler)(&dict_arg(&[(
        "session_id",
        vm_string(&fixture.session),
    )]))
    .unwrap();
    assert!(matches!(
        dict_get(&listed_after, "snapshots"),
        VmValue::List(items) if items.is_empty()
    ));
}

#[test]
fn auto_on_write_opens_current_tool_call_snapshot() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("plain.txt");
    let fixture = Fixture::new("snap-noop", true);
    let reg = registry();

    // No fs_snapshot call — the first write auto-opens the current tool-call
    // snapshot so session rollback can restore the pre-image later.
    (reg.find("hostlib_tools_write_file").unwrap().handler)(&dict_arg(&[
        ("path", vm_string(&path_str(&file))),
        ("content", vm_string("hi")),
    ]))
    .unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"hi");

    let listed = (reg.find("hostlib_fs_list_snapshots").unwrap().handler)(&dict_arg(&[(
        "session_id",
        vm_string(&fixture.session),
    )]))
    .unwrap();
    let snapshots = match dict_get(&listed, "snapshots") {
        VmValue::List(items) => items,
        other => panic!("snapshots not a list: {other:?}"),
    };
    assert_eq!(snapshots.len(), 1);

    (reg.find("hostlib_fs_restore").unwrap().handler)(&dict_arg(&[
        ("session_id", vm_string(&fixture.session)),
        ("snapshot_id", vm_string(&fixture.scope)),
    ]))
    .unwrap();
    assert!(
        !file.exists(),
        "restore deletes files created during the call"
    );
}

use super::*;
use std::sync::{Arc, LazyLock, Mutex};

static LONG_RUNNING_TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn vm() -> Vm {
    let mut vm = Vm::new();
    register_fs_builtins(&mut vm);
    vm
}

fn call(vm: &mut Vm, name: &str, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let f = vm.builtins.get(name).unwrap().clone();
    let mut out = String::new();
    f(&args, &mut out)
}

fn s(v: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(v))
}

fn b(v: bool) -> VmValue {
    VmValue::Bool(v)
}

fn dict(entries: Vec<(&str, VmValue)>) -> VmValue {
    VmValue::dict(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect::<crate::value::DictMap>(),
    )
}

fn field<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
    match value {
        VmValue::Dict(fields) => fields
            .get(key)
            .unwrap_or_else(|| panic!("missing field {key}")),
        other => panic!("expected dict, got {other:?}"),
    }
}

fn result_error(value: &VmValue) -> &VmValue {
    match value {
        VmValue::EnumVariant(result) if result.is_variant("Result", "Err") => result
            .fields
            .first()
            .expect("Result.Err must contain an error payload"),
        other => panic!("expected Result.Err, got {other:?}"),
    }
}

fn result_value(value: &VmValue) -> &VmValue {
    match value {
        VmValue::EnumVariant(result) if result.is_variant("Result", "Ok") => result
            .fields
            .first()
            .expect("Result.Ok must contain a value"),
        other => panic!("expected Result.Ok, got {other:?}"),
    }
}

#[test]
fn conditional_text_replacement_returns_closed_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let _locks =
        crate::conditional_replace::scope_conditional_replace_lock_root(dir.path().join("locks"));
    let path = dir.path().join("state.txt");
    let path_arg = path.to_string_lossy().into_owned();
    let mut vm = vm();

    let created = call(&mut vm, "replace_file", vec![s(&path_arg), s("one")]).unwrap();
    assert_eq!(field(&created, "status").display(), "created");
    assert_eq!(field(&created, "durability").display(), "namespace");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one");

    let expected = field(&created, "after_sha256").display();
    std::fs::write(&path, b"external").unwrap();
    let stale_result = call(
        &mut vm,
        "replace_file_result",
        vec![
            s(&path_arg),
            s("two"),
            dict(vec![("expected_sha256", s(&expected))]),
        ],
    )
    .unwrap();
    let stale = result_value(&stale_result);
    assert_eq!(field(stale, "status").display(), "stale");
    assert_eq!(field(stale, "bytes_written").display(), "0");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "external");
}

#[test]
fn conditional_byte_replacement_is_hermetic_under_overlay() {
    let dir = tempfile::tempdir().unwrap();
    let _locks =
        crate::conditional_replace::scope_conditional_replace_lock_root(dir.path().join("locks"));
    let overlay = Arc::new(crate::testbench::overlay_fs::OverlayFs::rooted_at(
        dir.path(),
    ));
    let _guard = crate::testbench::overlay_fs::install_overlay(overlay);
    let path = dir.path().join("state.bin");
    let path_arg = path.to_string_lossy().into_owned();
    let mut vm = vm();

    let receipt = call(
        &mut vm,
        "replace_file_bytes",
        vec![s(&path_arg), VmValue::Bytes(Arc::new(vec![0, 1, 2, 255]))],
    )
    .unwrap();
    assert_eq!(field(&receipt, "status").display(), "created");
    assert!(!path.exists());
    let VmValue::Bytes(bytes) = call(&mut vm, "read_file_bytes", vec![s(&path_arg)]).unwrap()
    else {
        panic!("read_file_bytes must return bytes");
    };
    assert_eq!(bytes.as_slice(), &[0, 1, 2, 255]);
}

#[test]
fn conditional_replacement_rejects_invalid_policy_options() {
    let dir = tempfile::tempdir().unwrap();
    let _locks =
        crate::conditional_replace::scope_conditional_replace_lock_root(dir.path().join("locks"));
    let path = dir.path().join("state.txt");
    let mut vm = vm();
    let result = call(
        &mut vm,
        "replace_file_result",
        vec![
            s(&path.to_string_lossy()),
            s("new"),
            dict(vec![("durability", s("eventually"))]),
        ],
    )
    .unwrap();
    assert_eq!(
        field(result_error(&result), "kind").display(),
        "invalid_input"
    );
    assert!(!path.exists());
}

#[test]
fn append_file_locked_creates_parent_dirs_and_honors_advisory_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("logs/deep/coord.ndjson");
    let path_arg = path.to_string_lossy().into_owned();
    let mut vm = vm();

    call(
        &mut vm,
        "append_file_locked",
        vec![s(&path_arg), s("one\n")],
    )
    .unwrap();
    call(
        &mut vm,
        "append_file_locked",
        vec![s(&path_arg), s("two\n")],
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");

    let holder = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .open(&path)
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&holder).unwrap();
    let clock = crate::stdlib::clock::MockClockGuard::install(0);
    let error = call(
        &mut vm,
        "append_file_locked",
        vec![
            s(&path_arg),
            s("blocked\n"),
            dict(vec![("timeout_ms", VmValue::Int(25))]),
        ],
    )
    .expect_err("held advisory lock must make locked append time out");
    match error {
        VmError::Thrown(value) => {
            assert_eq!(field(&value, "kind").display(), "timed_out");
        }
        other => panic!("expected structured io error, got {other:?}"),
    }
    assert_eq!(clock.now_monotonic_ms(), 25);
    drop(clock);
    fs2::FileExt::unlock(&holder).unwrap();

    call(
        &mut vm,
        "append_file_locked",
        vec![s(&path_arg), s("three\n")],
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\nthree\n");
}

fn drain_feedback(session_id: &str, handle_id: &str) -> serde_json::Value {
    const FEEDBACK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

    let mut seen_handles = Vec::new();
    for _ in 0..4 {
        for entry in crate::orchestration::agent_inbox::drain(session_id) {
            assert_eq!(entry.kind, "tool_result");
            let payload: serde_json::Value = serde_json::from_str(&entry.content).unwrap();
            if payload["handle_id"] == handle_id {
                return payload;
            }
            if let Some(seen) = payload["handle_id"].as_str() {
                seen_handles.push(seen.to_string());
            }
        }
        assert!(
            crate::orchestration::agent_inbox::wait_sync(session_id, FEEDBACK_WAIT),
            "timed out waiting for feedback for {handle_id}; saw handles {seen_handles:?}"
        );
    }
    panic!("timed out waiting for feedback for {handle_id}; saw handles {seen_handles:?}");
}

#[test]
fn file_exists_outside_sandbox_reads_as_absent_not_error() {
    use crate::orchestration::{
        pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
    };

    // An out-of-root presence probe must read as "absent" (false) rather
    // than throwing a sandbox violation, so eval/setup pipelines that
    // check for paths outside the workspace don't crash. Reading the
    // *content* of the same out-of-root path must still be denied.
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("present.txt");
    std::fs::write(&outside_file, "secret").unwrap();
    let outside_arg = outside_file.to_string_lossy().into_owned();

    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });

    let mut vm = vm();

    // file_exists on a real-but-out-of-root file returns false, no error.
    assert!(
        matches!(
            call(&mut vm, "file_exists", vec![s(&outside_arg)]).unwrap(),
            VmValue::Bool(false)
        ),
        "file_exists outside the sandbox must read as absent"
    );

    // Reading its content is still denied with a sandbox violation.
    let read_err = call(&mut vm, "read_file", vec![s(&outside_arg)])
        .expect_err("reading content outside the sandbox must still be denied");
    match read_err {
        VmError::CategorizedError { message, category } => {
            assert_eq!(category, crate::value::ErrorCategory::ToolRejected);
            assert!(message.contains("sandbox violation"));
        }
        other => panic!("expected a categorized sandbox denial, got {other:?}"),
    }

    // A path inside the workspace still reports its true presence.
    let inside = workspace.path().join("inside.txt");
    std::fs::write(&inside, "ok").unwrap();
    assert!(
        matches!(
            call(&mut vm, "file_exists", vec![s(&inside.to_string_lossy())]).unwrap(),
            VmValue::Bool(true)
        ),
        "file_exists inside the workspace must report true"
    );

    pop_execution_policy();
}

#[test]
fn path_status_distinguishes_missing_scope_and_read_only_denials() {
    use crate::orchestration::{
        clear_execution_policy_stacks, pop_execution_policy, push_execution_policy,
        CapabilityPolicy, SandboxProfile,
    };

    clear_execution_policy_stacks();
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let readonly = tempfile::tempdir().unwrap();
    let inside_file = workspace.path().join("inside.txt");
    let inside_dir = workspace.path().join("dir");
    let outside_file = outside.path().join("present.txt");
    let readonly_file = readonly.path().join("read-only.txt");
    std::fs::write(&inside_file, "ok").unwrap();
    std::fs::create_dir(&inside_dir).unwrap();
    std::fs::write(&outside_file, "secret").unwrap();
    std::fs::write(&readonly_file, "readonly").unwrap();

    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        read_only_roots: vec![readonly.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });

    let mut vm = vm();
    let file_status = call(
        &mut vm,
        "path_status",
        vec![s(&inside_file.to_string_lossy())],
    )
    .unwrap();
    assert_eq!(field(&file_status, "status").display(), "present_file");
    assert_eq!(field(&file_status, "kind").display(), "file");
    assert!(matches!(field(&file_status, "exists"), VmValue::Bool(true)));

    let dir_status = call(
        &mut vm,
        "path_status",
        vec![s(&inside_dir.to_string_lossy())],
    )
    .unwrap();
    assert_eq!(field(&dir_status, "status").display(), "present_dir");
    assert_eq!(field(&dir_status, "kind").display(), "dir");

    let missing_status = call(
        &mut vm,
        "path_status",
        vec![s(&workspace.path().join("missing.txt").to_string_lossy())],
    )
    .unwrap();
    assert_eq!(field(&missing_status, "status").display(), "missing");
    assert!(matches!(
        field(&missing_status, "exists"),
        VmValue::Bool(false)
    ));

    let outside_status = call(
        &mut vm,
        "path_status",
        vec![s(&outside_file.to_string_lossy())],
    )
    .unwrap();
    assert_eq!(field(&outside_status, "status").display(), "scope_denied");
    assert!(matches!(
        field(&outside_status, "visible"),
        VmValue::Bool(false)
    ));
    assert!(field(&outside_status, "message")
        .display()
        .contains("outside workspace_roots"));

    let readonly_status = call(
        &mut vm,
        "path_status",
        vec![s(&readonly_file.to_string_lossy()), s("write")],
    )
    .unwrap();
    assert_eq!(
        field(&readonly_status, "status").display(),
        "read_only_denied"
    );
    assert!(matches!(
        field(&readonly_status, "read_only"),
        VmValue::Bool(true)
    ));

    pop_execution_policy();
    clear_execution_policy_stacks();
}

#[test]
fn workspace_temp_dir_is_inside_sandbox_workspace() {
    use crate::orchestration::{
        clear_execution_policy_stacks, pop_execution_policy, push_execution_policy,
        CapabilityPolicy, SandboxProfile,
    };

    clear_execution_policy_stacks();
    let workspace = tempfile::tempdir().unwrap();
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });

    let mut vm = vm();
    let root = call(&mut vm, "workspace_temp_dir", vec![])
        .unwrap()
        .display();
    let child = call(&mut vm, "mkdtemp_in_workspace", vec![s("receipts/")])
        .unwrap()
        .display();

    pop_execution_policy();
    clear_execution_policy_stacks();

    let root = PathBuf::from(root);
    let child = PathBuf::from(child);
    // The temp-dir builtins now return de-verbatimed paths (a `\\?\` prefix
    // breaks the documented `dir + "/child"` join on Windows), so normalize the
    // expected workspace root the same way before comparing components —
    // otherwise `\\?\C:\…` vs `C:\…` fails `starts_with` on Windows only.
    let workspace_root = PathBuf::from(temp_dir_path_string(
        &workspace.path().canonicalize().unwrap(),
    ));
    assert!(root.starts_with(&workspace_root));
    assert!(root.ends_with(crate::stdlib::sandbox::WORKSPACE_TMPDIR_NAME));
    assert!(root.is_dir());
    assert!(root.join(".gitignore").is_file());
    assert!(child.starts_with(&root));
    assert!(child.is_dir());
    assert!(child
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("receipts")));
}

#[test]
fn read_file_loads_embedded_stdlib_prompt_source() {
    let mut vm = vm();
    let source = call(
        &mut vm,
        "read_file",
        vec![s("std/agent/prompts/tool_contract_text.harn.prompt")],
    )
    .unwrap()
    .display();
    assert!(source.contains("{{ text_response_protocol }}"));
    assert!(source.contains("{{ native_contract }}"));
}

#[test]
fn read_file_cache_invalidates_after_external_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.txt");
    std::fs::write(&path, "one").unwrap();
    let path_arg = path.to_string_lossy().into_owned();
    let mut vm = vm();

    assert_eq!(
        call(&mut vm, "read_file", vec![s(&path_arg)])
            .unwrap()
            .display(),
        "one"
    );
    std::fs::write(&path, "two updated").unwrap();

    assert_eq!(
        call(&mut vm, "read_file", vec![s(&path_arg)])
            .unwrap()
            .display(),
        "two updated"
    );
}

#[test]
fn list_dir_observes_active_overlay_entries() {
    let dir = tempfile::tempdir().unwrap();
    let overlay = std::sync::Arc::new(crate::testbench::overlay_fs::OverlayFs::rooted_at(
        dir.path(),
    ));
    let _guard = crate::testbench::overlay_fs::install_overlay(overlay);
    let mut vm = vm();
    let subdir = dir.path().join("overlay-dir");
    let file = subdir.join("created.txt");

    call(&mut vm, "mkdir", vec![s(&subdir.to_string_lossy())]).unwrap();
    call(
        &mut vm,
        "write_file",
        vec![s(&file.to_string_lossy()), s("overlay only")],
    )
    .unwrap();

    let listed = call(&mut vm, "list_dir", vec![s(&subdir.to_string_lossy())]).unwrap();
    let VmValue::List(items) = listed else {
        panic!("list_dir returns list");
    };
    let names = items.iter().map(VmValue::display).collect::<Vec<_>>();
    assert_eq!(names, vec!["created.txt".to_string()]);
    assert!(
        !file.exists(),
        "overlay write should not materialize on the underlying fs"
    );
}

#[test]
fn mkdir_defaults_recursive_and_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("one/two/three");
    let nested_arg = nested.to_string_lossy().into_owned();
    let mut vm = vm();

    call(&mut vm, "mkdir", vec![s(&nested_arg)]).unwrap();
    assert!(
        nested.is_dir(),
        "default mkdir should create missing parents"
    );
    call(&mut vm, "mkdir", vec![s(&nested_arg)]).unwrap();
    assert!(nested.is_dir(), "default mkdir should remain idempotent");
}

#[test]
fn mkdir_nonrecursive_requires_existing_parent_and_unique_target() {
    let dir = tempfile::tempdir().unwrap();
    let missing_parent = dir.path().join("missing/lock.d");
    let missing_parent_arg = missing_parent.to_string_lossy().into_owned();
    let mut vm = vm();

    let missing_parent_err = call(&mut vm, "mkdir", vec![s(&missing_parent_arg), b(false)])
        .expect_err("recursive=false must not create missing parents");
    assert!(
        missing_parent_err
            .to_string()
            .contains("Failed to create directory"),
        "unexpected mkdir error: {missing_parent_err}"
    );
    assert!(
        !missing_parent.exists(),
        "recursive=false must not materialize nested path after failure"
    );

    let parent = dir.path().join("locks");
    std::fs::create_dir(&parent).unwrap();
    let lock = parent.join("release.lock.d");
    let lock_arg = lock.to_string_lossy().into_owned();
    call(&mut vm, "mkdir", vec![s(&lock_arg), b(false)]).unwrap();
    assert!(
        lock.is_dir(),
        "recursive=false should create exactly one directory"
    );

    let duplicate_err = call(&mut vm, "mkdir", vec![s(&lock_arg), b(false)])
        .expect_err("recursive=false must fail when the target already exists");
    assert!(
        duplicate_err
            .to_string()
            .contains("Failed to create directory"),
        "unexpected duplicate mkdir error: {duplicate_err}"
    );
}

#[test]
fn mkdir_nonrecursive_observes_active_overlay_entries() {
    let dir = tempfile::tempdir().unwrap();
    let overlay = std::sync::Arc::new(crate::testbench::overlay_fs::OverlayFs::rooted_at(
        dir.path(),
    ));
    let _guard = crate::testbench::overlay_fs::install_overlay(overlay);
    let mut vm = vm();
    let lock = dir.path().join("release.lock.d");
    let lock_arg = lock.to_string_lossy().into_owned();

    call(&mut vm, "mkdir", vec![s(&lock_arg), b(false)]).unwrap();
    assert!(
        !lock.exists(),
        "overlay nonrecursive mkdir should not materialize on the underlying fs"
    );
    assert!(
        matches!(
            call(&mut vm, "file_exists", vec![s(&lock_arg)]).unwrap(),
            VmValue::Bool(true)
        ),
        "overlay nonrecursive mkdir should be visible through harness fs"
    );

    let duplicate_err = call(&mut vm, "mkdir", vec![s(&lock_arg), b(false)])
        .expect_err("overlay recursive=false mkdir must fail when target exists in overlay");
    assert!(
        duplicate_err
            .to_string()
            .contains("Failed to create directory"),
        "unexpected overlay duplicate mkdir error: {duplicate_err}"
    );
}

#[test]
fn read_lines_observes_active_overlay_content() {
    let dir = tempfile::tempdir().unwrap();
    let overlay = std::sync::Arc::new(crate::testbench::overlay_fs::OverlayFs::rooted_at(
        dir.path(),
    ));
    let _guard = crate::testbench::overlay_fs::install_overlay(overlay);
    let mut vm = vm();
    let path = dir.path().join("lines.txt");

    call(
        &mut vm,
        "write_file",
        vec![s(&path.to_string_lossy()), s("one\ntwo\n")],
    )
    .unwrap();

    let lines = call(&mut vm, "read_lines", vec![s(&path.to_string_lossy())]).unwrap();
    let VmValue::List(items) = lines else {
        panic!("read_lines returns list");
    };
    let lines = items.iter().map(VmValue::display).collect::<Vec<_>>();
    assert_eq!(lines, vec!["one".to_string(), "two".to_string()]);
    assert!(
        !path.exists(),
        "overlay write should not materialize on the underlying fs"
    );
}

#[test]
fn copy_file_observes_active_overlay_content() {
    let dir = tempfile::tempdir().unwrap();
    let overlay = std::sync::Arc::new(crate::testbench::overlay_fs::OverlayFs::rooted_at(
        dir.path(),
    ));
    let _guard = crate::testbench::overlay_fs::install_overlay(overlay);
    let mut vm = vm();
    let src = dir.path().join("src.txt");
    let dst = dir.path().join("dst.txt");

    call(
        &mut vm,
        "write_file",
        vec![s(&src.to_string_lossy()), s("overlay only")],
    )
    .unwrap();
    call(
        &mut vm,
        "copy_file",
        vec![s(&src.to_string_lossy()), s(&dst.to_string_lossy())],
    )
    .unwrap();

    assert_eq!(
        call(&mut vm, "read_file", vec![s(&dst.to_string_lossy())])
            .unwrap()
            .display(),
        "overlay only"
    );
    assert!(!dst.exists(), "overlay copy should not touch real disk");
}

#[test]
fn move_file_observes_active_overlay_content() {
    let dir = tempfile::tempdir().unwrap();
    let overlay = std::sync::Arc::new(crate::testbench::overlay_fs::OverlayFs::rooted_at(
        dir.path(),
    ));
    let _guard = crate::testbench::overlay_fs::install_overlay(overlay);
    let mut vm = vm();
    let src = dir.path().join("src.txt");
    let dst = dir.path().join("dst.txt");

    call(
        &mut vm,
        "write_file",
        vec![s(&src.to_string_lossy()), s("overlay only")],
    )
    .unwrap();
    call(
        &mut vm,
        "move_file",
        vec![s(&src.to_string_lossy()), s(&dst.to_string_lossy())],
    )
    .unwrap();

    assert_eq!(
        call(&mut vm, "read_file", vec![s(&dst.to_string_lossy())])
            .unwrap()
            .display(),
        "overlay only"
    );
    assert!(call(&mut vm, "read_file", vec![s(&src.to_string_lossy())]).is_err());
    assert!(!dst.exists(), "overlay move should not touch real disk");
}

#[test]
fn fs_mutations_emit_file_edited_notifications() {
    crate::orchestration::clear_file_edit_queue();
    let dir = tempfile::tempdir().unwrap();
    let mut vm = vm();
    let src = dir.path().join("src.txt");
    let dst = dir.path().join("dst.txt");
    let moved = dir.path().join("moved.txt");
    let subdir = dir.path().join("subdir");

    call(
        &mut vm,
        "write_file",
        vec![s(&src.to_string_lossy()), s("hi")],
    )
    .unwrap();
    call(
        &mut vm,
        "copy_file",
        vec![s(&src.to_string_lossy()), s(&dst.to_string_lossy())],
    )
    .unwrap();
    call(
        &mut vm,
        "move_file",
        vec![s(&dst.to_string_lossy()), s(&moved.to_string_lossy())],
    )
    .unwrap();
    call(&mut vm, "mkdir", vec![s(&subdir.to_string_lossy())]).unwrap();
    call(&mut vm, "delete_file", vec![s(&moved.to_string_lossy())]).unwrap();

    let operations = crate::orchestration::drain_file_edits()
        .into_iter()
        .filter_map(|entry| {
            entry
                .metadata
                .get("operation")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    assert!(operations.contains(&"write".to_string()));
    assert!(operations.contains(&"copy".to_string()));
    assert!(operations.contains(&"move".to_string()));
    assert!(operations.contains(&"mkdir".to_string()));
    assert!(operations.contains(&"delete".to_string()));
}

#[test]
fn walk_dir_long_running_returns_handle_and_feedback() {
    // Recover from a poisoned mutex: each test calls
    // `reset_state()` immediately below, so the previous test's
    // panic-shaped failure does not contaminate this one. Without
    // this, a single failing test cascades into PoisonError-driven
    // failures across every subsequent long-running test.
    let _guard = LONG_RUNNING_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _inbox_reset_guard = crate::orchestration::agent_inbox::lock_reset_for_test();
    crate::stdlib::long_running::reset_state();
    let session_id = format!("fs-long-running-{}", uuid::Uuid::now_v7());
    let _session_guard = crate::agent_sessions::enter_current_session(session_id.clone());
    let _ = crate::orchestration::agent_inbox::drain(&session_id);
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.harn"), "fn main() {}\n").unwrap();
    let mut vm = vm();

    let response = call(
        &mut vm,
        "walk_dir",
        vec![
            s(&dir.path().to_string_lossy()),
            dict(vec![("long_running", b(true))]),
        ],
    )
    .unwrap();
    let response = response.as_dict().expect("handle dict");
    assert_eq!(response["status"].display(), "running");
    assert_eq!(response["operation"].display(), "walk_dir");
    assert!(response["command_or_op_descriptor"]
        .display()
        .contains("walk_dir"));
    let handle_id = response["handle_id"].display();
    let payload = drain_feedback(&session_id, &handle_id);

    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["operation"], "walk_dir");
    assert!(payload["result"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["path"].as_str().unwrap().ends_with("src/lib.harn")));
}

#[test]
fn glob_long_running_returns_handle_and_feedback() {
    // Recover from a poisoned mutex: each test calls
    // `reset_state()` immediately below, so the previous test's
    // panic-shaped failure does not contaminate this one. Without
    // this, a single failing test cascades into PoisonError-driven
    // failures across every subsequent long-running test.
    let _guard = LONG_RUNNING_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _inbox_reset_guard = crate::orchestration::agent_inbox::lock_reset_for_test();
    crate::stdlib::long_running::reset_state();
    let session_id = format!("fs-long-running-{}", uuid::Uuid::now_v7());
    let _session_guard = crate::agent_sessions::enter_current_session(session_id.clone());
    let _ = crate::orchestration::agent_inbox::drain(&session_id);
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.harn"), "fn main() {}\n").unwrap();
    std::fs::write(dir.path().join("README.md"), "# test\n").unwrap();
    let mut vm = vm();

    let response = call(
        &mut vm,
        "glob",
        vec![
            s("**/*.harn"),
            s(&dir.path().to_string_lossy()),
            dict(vec![("background", b(true))]),
        ],
    )
    .unwrap();
    let response = response.as_dict().expect("handle dict");
    assert_eq!(response["status"].display(), "running");
    assert_eq!(response["operation"].display(), "glob");
    let handle_id = response["handle_id"].display();
    let payload = drain_feedback(&session_id, &handle_id);

    assert_eq!(payload["status"], "completed");
    let result = payload["result"].as_array().unwrap();
    assert_eq!(result.len(), 1);
    assert!(result[0].as_str().unwrap().ends_with("src/lib.harn"));
}

#[test]
fn find_text_returns_structured_hits() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.harn"),
        "alpha\nneedle here\nanother needle\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("README.md"), "needle docs\n").unwrap();
    let mut vm = vm();

    let response = call(
        &mut vm,
        "find_text",
        vec![s(&dir.path().to_string_lossy()), s("needle")],
    )
    .unwrap();
    let VmValue::List(hits) = response else {
        panic!("find_text returns list");
    };
    assert_eq!(hits.len(), 3);
    let first = hits[0].as_dict().expect("hit dict");
    assert!(first["path"].display().ends_with("README.md"));
    assert_eq!(first["line"].as_int(), Some(1));
    assert_eq!(first["col"].as_int(), Some(1));
    assert_eq!(first["column"].as_int(), Some(1));
    assert_eq!(first["text"].display(), "needle docs");
}

#[test]
fn find_text_filters_case_and_limits() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.harn"), "Needle\nneedle\n").unwrap();
    std::fs::write(dir.path().join("src/b.txt"), "needle\n").unwrap();
    let mut vm = vm();

    let response = call(
        &mut vm,
        "find_text",
        vec![
            s(&dir.path().to_string_lossy()),
            s("needle"),
            dict(vec![
                ("include", s("**/*.harn")),
                ("case_insensitive", b(true)),
                ("max_matches", VmValue::Int(1)),
            ]),
        ],
    )
    .unwrap();
    let VmValue::List(hits) = response else {
        panic!("find_text returns list");
    };
    assert_eq!(hits.len(), 1);
    let hit = hits[0].as_dict().expect("hit dict");
    assert!(hit["path"].display().ends_with("src/a.harn"));
    assert_eq!(hit["text"].display(), "Needle");
}

#[test]
fn find_text_summary_modes_and_source_preset() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
    std::fs::write(dir.path().join("src/a.harn"), "needle\nneedle\n").unwrap();
    std::fs::write(dir.path().join("node_modules/pkg/a.harn"), "needle\n").unwrap();
    let mut vm = vm();

    let exists = call(
        &mut vm,
        "find_text",
        vec![
            s(&dir.path().to_string_lossy()),
            s("needle"),
            dict(vec![
                ("mode", s("exists")),
                ("preset", s("source")),
                ("parallel", b(true)),
            ]),
        ],
    )
    .unwrap();
    assert!(matches!(exists, VmValue::Bool(true)));

    let count = call(
        &mut vm,
        "find_text",
        vec![
            s(&dir.path().to_string_lossy()),
            s("needle"),
            dict(vec![
                ("mode", s("count")),
                ("preset", s("source")),
                ("parallel", b(true)),
            ]),
        ],
    )
    .unwrap();
    assert_eq!(count.as_int(), Some(2));
}

#[test]
fn find_text_long_running_returns_handle_and_feedback() {
    let _guard = LONG_RUNNING_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _inbox_reset_guard = crate::orchestration::agent_inbox::lock_reset_for_test();
    crate::stdlib::long_running::reset_state();
    let session_id = format!("fs-long-running-{}", uuid::Uuid::now_v7());
    let _session_guard = crate::agent_sessions::enter_current_session(session_id.clone());
    let _ = crate::orchestration::agent_inbox::drain(&session_id);
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.harn"), "needle\n").unwrap();
    let mut vm = vm();

    let response = call(
        &mut vm,
        "find_text",
        vec![
            s(&dir.path().to_string_lossy()),
            s("needle"),
            dict(vec![("background", b(true))]),
        ],
    )
    .unwrap();
    let response = response.as_dict().expect("handle dict");
    assert_eq!(response["status"].display(), "running");
    assert_eq!(response["operation"].display(), "find_text");
    let handle_id = response["handle_id"].display();
    let payload = drain_feedback(&session_id, &handle_id);

    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["operation"], "find_text");
    let result = payload["result"].as_array().unwrap();
    assert_eq!(result.len(), 1);
    assert!(result[0]["path"]
        .as_str()
        .unwrap()
        .ends_with("src/lib.harn"));
    assert_eq!(result[0]["line"], 1);
    assert_eq!(result[0]["col"], 1);
}

fn evidence_root(id: &str, path: &std::path::Path) -> VmValue {
    dict(vec![("id", s(id)), ("path", s(&path.to_string_lossy()))])
}

fn evidence_pattern(id: &str, text: &str) -> VmValue {
    dict(vec![("id", s(id)), ("text", s(text))])
}

#[test]
fn find_evidence_is_labeled_overlapping_and_worker_deterministic() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    std::fs::write(first.path().join("a.txt"), "needle\n").unwrap();
    std::fs::write(second.path().join("b.txt"), "a needle twice needle\n").unwrap();
    let roots = VmValue::List(Arc::new(vec![
        evidence_root("z-root", second.path()),
        evidence_root("a-root", first.path()),
    ]));
    let patterns = VmValue::List(Arc::new(vec![
        evidence_pattern("whole", "needle"),
        evidence_pattern("prefix", "need"),
    ]));
    let mut vm = vm();

    let sequential = call(
        &mut vm,
        "find_evidence",
        vec![
            roots.clone(),
            patterns.clone(),
            dict(vec![("threads", VmValue::Int(1))]),
        ],
    )
    .unwrap();
    let parallel = call(
        &mut vm,
        "find_evidence",
        vec![roots, patterns, dict(vec![("threads", VmValue::Int(4))])],
    )
    .unwrap();

    assert_eq!(
        crate::llm::vm_value_to_json(&sequential),
        crate::llm::vm_value_to_json(&parallel)
    );
    assert_eq!(field(&parallel, "status").display(), "complete");
    assert_eq!(field(&parallel, "match_count").as_int(), Some(6));
    assert_eq!(field(&parallel, "files_scanned").as_int(), Some(2));
    let VmValue::List(roots) = field(&parallel, "roots") else {
        panic!("roots must be a list");
    };
    assert_eq!(field(&roots[0], "id").display(), "a-root");
    let VmValue::List(groups) = field(&roots[1], "patterns") else {
        panic!("patterns must be a list");
    };
    assert_eq!(field(&groups[0], "id").display(), "prefix");
    assert_eq!(field(&groups[0], "count").as_int(), Some(2));
    assert_eq!(field(&groups[1], "count").as_int(), Some(2));
}

#[cfg(unix)]
#[test]
fn find_evidence_follows_symlinks_only_when_requested() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    std::fs::write(external.path().join("linked.txt"), "needle\n").unwrap();
    symlink(external.path(), root.path().join("linked")).unwrap();
    let roots = VmValue::List(Arc::new(vec![evidence_root("repo", root.path())]));
    let patterns = VmValue::List(Arc::new(vec![evidence_pattern("needle", "needle")]));
    let mut vm = vm();

    let default_receipt = call(
        &mut vm,
        "find_evidence",
        vec![roots.clone(), patterns.clone()],
    )
    .unwrap();
    assert_eq!(field(&default_receipt, "match_count").as_int(), Some(0));

    let followed = call(
        &mut vm,
        "find_evidence",
        vec![roots, patterns, dict(vec![("follow_symlinks", b(true))])],
    )
    .unwrap();
    assert_eq!(field(&followed, "status").display(), "complete");
    assert_eq!(field(&followed, "match_count").as_int(), Some(1));
    let VmValue::List(roots) = field(&followed, "roots") else {
        panic!("roots must be a list");
    };
    let VmValue::List(groups) = field(&roots[0], "patterns") else {
        panic!("patterns must be a list");
    };
    let VmValue::List(hits) = field(&groups[0], "hits") else {
        panic!("hits must be a list");
    };
    assert_eq!(field(&hits[0], "path").display(), "linked/linked.txt");
}

#[test]
fn find_evidence_long_running_returns_typed_feedback() {
    let _guard = LONG_RUNNING_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _inbox_reset_guard = crate::orchestration::agent_inbox::lock_reset_for_test();
    crate::stdlib::long_running::reset_state();
    let session_id = format!("fs-evidence-long-running-{}", uuid::Uuid::now_v7());
    let _session_guard = crate::agent_sessions::enter_current_session(session_id.clone());
    let _ = crate::orchestration::agent_inbox::drain(&session_id);
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("source.harn"), "needle\n").unwrap();
    let mut vm = vm();

    let response = call(
        &mut vm,
        "find_evidence",
        vec![
            VmValue::List(Arc::new(vec![evidence_root("repo", root.path())])),
            VmValue::List(Arc::new(vec![evidence_pattern("needle", "needle")])),
            dict(vec![("background", b(true))]),
        ],
    )
    .unwrap();
    let response = response.as_dict().expect("handle dict");
    assert_eq!(response["status"].display(), "running");
    assert_eq!(response["operation"].display(), "find_evidence");
    let payload = drain_feedback(&session_id, &response["handle_id"].display());

    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["operation"], "find_evidence");
    assert_eq!(payload["result"]["schema_version"], "harn.fs.evidence.v1");
    assert_eq!(payload["result"]["status"], "complete");
    assert_eq!(payload["result"]["match_count"], 1);
}

#[test]
fn find_evidence_settles_failed_roots_and_handles_non_utf8() {
    let readable = tempfile::tempdir().unwrap();
    std::fs::write(
        readable.path().join("bytes.bin"),
        b"before\xffneedle after\n",
    )
    .unwrap();
    std::fs::create_dir_all(readable.path().join("node_modules/pkg")).unwrap();
    std::fs::write(
        readable.path().join("node_modules/pkg/generated.js"),
        "needle\n",
    )
    .unwrap();
    let missing = readable.path().join("missing");
    let roots = VmValue::List(Arc::new(vec![
        evidence_root("missing", &missing),
        evidence_root("readable", readable.path()),
    ]));
    let patterns = VmValue::List(Arc::new(vec![evidence_pattern("needle", "needle")]));
    let mut vm = vm();

    let receipt = call(
        &mut vm,
        "find_evidence",
        vec![roots, patterns, dict(vec![("preset", s("source"))])],
    )
    .unwrap();
    assert_eq!(field(&receipt, "status").display(), "partial");
    assert_eq!(field(&receipt, "failed_root_count").as_int(), Some(1));
    assert_eq!(field(&receipt, "match_count").as_int(), Some(1));
    let VmValue::List(roots) = field(&receipt, "roots") else {
        panic!("roots must be a list");
    };
    assert_eq!(field(&roots[0], "status").display(), "failed");
    assert_eq!(field(&roots[1], "status").display(), "complete");
}

#[test]
fn find_evidence_applies_stable_global_and_root_budgets() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    std::fs::write(first.path().join("a.txt"), "x x x\n").unwrap();
    std::fs::write(second.path().join("b.txt"), "x x x\n").unwrap();
    let roots = VmValue::List(Arc::new(vec![
        evidence_root("b", second.path()),
        evidence_root("a", first.path()),
    ]));
    let patterns = VmValue::List(Arc::new(vec![evidence_pattern("x", "x")]));
    let mut vm = vm();

    let receipt = call(
        &mut vm,
        "find_evidence",
        vec![
            roots,
            patterns,
            dict(vec![
                ("max_matches", VmValue::Int(3)),
                ("max_matches_per_root", VmValue::Int(2)),
                ("threads", VmValue::Int(2)),
            ]),
        ],
    )
    .unwrap();
    assert_eq!(field(&receipt, "status").display(), "truncated");
    assert_eq!(field(&receipt, "match_count").as_int(), Some(3));
    let VmValue::List(roots) = field(&receipt, "roots") else {
        panic!("roots must be a list");
    };
    assert_eq!(field(&roots[0], "match_count").as_int(), Some(2));
    assert_eq!(field(&roots[1], "match_count").as_int(), Some(1));
}

#[test]
fn find_evidence_rejects_duplicate_or_empty_labels() {
    let root = tempfile::tempdir().unwrap();
    let duplicate_roots = VmValue::List(Arc::new(vec![
        evidence_root("same", root.path()),
        evidence_root("same", root.path()),
    ]));
    let patterns = VmValue::List(Arc::new(vec![evidence_pattern("p", "x")]));
    let mut vm = vm();
    let error = call(&mut vm, "find_evidence", vec![duplicate_roots, patterns]).unwrap_err();
    assert!(error.to_string().contains("duplicate roots id `same`"));

    let roots = VmValue::List(Arc::new(vec![evidence_root("root", root.path())]));
    let empty_patterns = VmValue::List(Arc::new(vec![evidence_pattern("", "x")]));
    let error = call(&mut vm, "find_evidence", vec![roots, empty_patterns]).unwrap_err();
    assert!(error.to_string().contains("patterns id must not be empty"));
}

#[test]
fn io_error_kind_str_maps_representative_kinds() {
    use std::io::{Error, ErrorKind};

    // Contract keys `.harn` consumers branch on (issue #4420). A handful of
    // representative kinds — including the two the atomic-write consumer needs
    // (storage_full / quota_exceeded) — plus the non_exhaustive catch-all.
    assert_eq!(
        io_error_kind_str(&Error::from(ErrorKind::NotFound)),
        "not_found"
    );
    assert_eq!(
        io_error_kind_str(&Error::from(ErrorKind::PermissionDenied)),
        "permission_denied"
    );
    assert_eq!(
        io_error_kind_str(&Error::from(ErrorKind::AlreadyExists)),
        "already_exists"
    );
    assert_eq!(
        io_error_kind_str(&Error::from(ErrorKind::StorageFull)),
        "storage_full"
    );
    assert_eq!(
        io_error_kind_str(&Error::from(ErrorKind::QuotaExceeded)),
        "quota_exceeded"
    );
    // An unnamed / platform-specific kind normalizes to "other" via the
    // required non_exhaustive catch-all rather than leaking a Debug string.
    assert_eq!(
        io_error_kind_str(&Error::from(ErrorKind::WriteZero)),
        "other"
    );
}

#[test]
fn io_error_value_is_structured_dict() {
    use std::io::{Error, ErrorKind};

    let value = io_error_value(
        "Failed to write file /tmp/x: disk full",
        &Error::from(ErrorKind::StorageFull),
    );
    assert_eq!(field(&value, "error").display(), "io_error");
    assert_eq!(field(&value, "kind").display(), "storage_full");
    // Message prose is preserved verbatim so a stringifying catch still reads.
    assert_eq!(
        field(&value, "message").display(),
        "Failed to write file /tmp/x: disk full"
    );
}

#[test]
fn read_file_missing_throws_typed_io_error() {
    use crate::orchestration::{
        clear_execution_policy_stacks, pop_execution_policy, push_execution_policy,
        CapabilityPolicy, SandboxProfile,
    };

    // A genuine io failure (NotFound) on an in-sandbox path lowers to the
    // structured `{error, kind, message}` dict — not a prose string — so
    // consumers branch on `kind == "not_found"`.
    clear_execution_policy_stacks();
    let workspace = tempfile::tempdir().unwrap();
    let missing = workspace.path().join("missing.txt");
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });

    let mut vm = vm();
    let err = call(&mut vm, "read_file", vec![s(&missing.to_string_lossy())])
        .expect_err("reading a missing file must fail");
    match err {
        VmError::Thrown(VmValue::Dict(dict)) => {
            assert_eq!(
                dict.get("error").map(|v| v.display()).as_deref(),
                Some("io_error")
            );
            assert_eq!(
                dict.get("kind").map(|v| v.display()).as_deref(),
                Some("not_found")
            );
            assert!(dict
                .get("message")
                .map(|v| v.display())
                .unwrap_or_default()
                .contains("Failed to read file"));
        }
        other => panic!("expected a structured io_error dict, got {other:?}"),
    }

    pop_execution_policy();
}

#[test]
fn read_file_result_preserves_structured_io_failure_kinds() {
    let workspace = tempfile::tempdir().unwrap();
    let missing = workspace.path().join("missing.json");
    let invalid_utf8 = workspace.path().join("invalid-utf8.json");
    std::fs::write(&invalid_utf8, [0xff, 0xfe]).unwrap();
    let mut vm = vm();

    for (path, expected_kind) in [
        (missing.as_path(), "not_found"),
        (invalid_utf8.as_path(), "invalid_data"),
    ] {
        let result = call(
            &mut vm,
            "read_file_result",
            vec![s(&path.to_string_lossy())],
        )
        .expect("read failures are returned as Result.Err");
        let failure = result_error(&result);
        assert_eq!(field(failure, "error").display(), "io_error");
        assert_eq!(field(failure, "kind").display(), expected_kind);
        assert!(!field(failure, "message").display().is_empty());
    }

    let directory = call(
        &mut vm,
        "read_file_result",
        vec![s(&workspace.path().to_string_lossy())],
    )
    .expect("directory read failure is returned as Result.Err");
    let directory_kind = field(result_error(&directory), "kind").display();
    assert!(
        matches!(
            directory_kind.as_str(),
            "is_a_directory" | "permission_denied" | "other"
        ),
        "directory read must stay an unreadable I/O kind, got {directory_kind}"
    );
}

#[test]
fn read_file_result_returns_sandbox_denial_instead_of_absence() {
    use crate::orchestration::{
        clear_execution_policy_stacks, pop_execution_policy, push_execution_policy,
        CapabilityPolicy, SandboxProfile,
    };

    clear_execution_policy_stacks();
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("present.json");
    std::fs::write(&outside_file, "{}").unwrap();
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });

    let mut vm = vm();
    let result = call(
        &mut vm,
        "read_file_result",
        vec![s(&outside_file.to_string_lossy())],
    )
    .expect("sandbox failures are returned as Result.Err");
    assert_eq!(
        field(result_error(&result), "kind").display(),
        "sandbox_denied"
    );
    assert_eq!(
        field(result_error(&result), "category").display(),
        "tool_rejected"
    );

    pop_execution_policy();
}

#[test]
fn unknown_prompt_asset_is_a_typed_not_found_result() {
    let mut vm = vm();
    let result = call(
        &mut vm,
        "read_file_result",
        vec![s("std/does-not-exist.harn.prompt")],
    )
    .expect("unknown assets are returned as Result.Err");
    assert_eq!(field(result_error(&result), "kind").display(), "not_found");
}

#[test]
fn strip_windows_verbatim_prefix_normalizes_verbatim_paths() {
    // Drive/root verbatim path loses its `\\?\` prefix so a later `/`-join
    // resolves as a separator instead of a literal filename character.
    assert_eq!(
        strip_windows_verbatim_prefix(r"\\?\C:\Users\runner\Temp\harn-abc"),
        r"C:\Users\runner\Temp\harn-abc"
    );
    // UNC verbatim paths collapse back to the `\\server\share` form.
    assert_eq!(
        strip_windows_verbatim_prefix(r"\\?\UNC\server\share\dir"),
        r"\\server\share\dir"
    );
    // Already-normal paths (every well-formed POSIX path, plain Windows paths)
    // are returned unchanged.
    assert_eq!(
        strip_windows_verbatim_prefix("/tmp/harn-abc/child"),
        "/tmp/harn-abc/child"
    );
    assert_eq!(
        strip_windows_verbatim_prefix(r"C:\Temp\harn-abc"),
        r"C:\Temp\harn-abc"
    );
}

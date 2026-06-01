use super::*;
use std::sync::{LazyLock, Mutex};

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
    VmValue::String(std::sync::Arc::from(v))
}

fn b(v: bool) -> VmValue {
    VmValue::Bool(v)
}

fn dict(entries: Vec<(&str, VmValue)>) -> VmValue {
    VmValue::Dict(std::sync::Arc::new(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    ))
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
    let message = match read_err {
        VmError::Thrown(VmValue::String(text)) => text.to_string(),
        other => format!("{other:?}"),
    };
    assert!(
        message.contains("sandbox violation"),
        "expected a sandbox violation, got: {message}"
    );

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

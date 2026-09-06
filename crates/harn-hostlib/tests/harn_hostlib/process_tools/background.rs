//! Background process handles, feedback, waiting, and cancellation.
use super::*;

// -------- background handles --------

#[test]
fn run_command_background_returns_handle_immediately() {
    let _session_guard = harn_vm::agent_sessions::enter_current_session(unique_session_id(
        "test-run-command-long-running",
    ));
    // Stay running until the test cancels.
    let (_spawner, _controller, _guard) = install_mock_with(MockProcessConfig::running());

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("background".into(), VmValue::Bool(true));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    let handle_id = require_str(&resp, "handle_id");
    assert!(!handle_id.is_empty(), "handle_id must be non-empty");
    assert!(
        handle_id.starts_with("hto-"),
        "handle_id should start with hto-, got {handle_id}"
    );
    assert_eq!(require_str(&resp, "status"), "running");
    assert!(require_str(&resp, "command_id").starts_with("cmd_"));
    let output_path = require_str(&resp, "output_path");
    assert!(require_int(&resp, "pid") > 0);
    assert!(require_int(&resp, "process_group_id") > 0);
    assert!(require_str(&resp, "started_at").contains('T'));
    let cmd = require_str(&resp, "command");
    assert!(
        cmd.contains("sleep"),
        "command should contain 'sleep', got {cmd}"
    );

    let mut read_req = dict();
    read_req.insert("handle_id".into(), vstr(&handle_id));
    let read_resp = require_dict(call("hostlib_tools_read_command_output", read_req).unwrap());
    assert_eq!(require_str(&read_resp, "path"), output_path);
    assert_eq!(require_int(&read_resp, "total_bytes"), 0);
    assert_eq!(require_str(&read_resp, "content"), "");

    // Block on waiter completion before returning so the test stays
    // deterministic — cancel signals the notifier itself.
    let completion_rx = register_completion_notifier(&handle_id);

    // Clean up: cancel so the waiter unblocks.
    let mut cancel_req = dict();
    cancel_req.insert("handle_id".into(), vstr(&handle_id));
    let cancel_resp = require_dict(call("hostlib_tools_cancel_handle", cancel_req).unwrap());
    assert!(require_bool(&cancel_resp, "cancelled"));

    if let Some(rx) = completion_rx {
        let _ = rx.recv();
    }
}

#[test]
fn run_command_background_reports_nil_process_group_when_unavailable() {
    let _session_guard = harn_vm::agent_sessions::enter_current_session(unique_session_id(
        "test-run-command-long-running-no-pgid",
    ));
    let config = MockProcessConfig {
        pgid: None,
        ..MockProcessConfig::running()
    };
    let (_spawner, _controller, _guard) = install_mock_with(config);

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("background".into(), VmValue::Bool(true));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    assert_eq!(require_str(&resp, "status"), "running");
    require_nil(&resp, "process_group_id");
    let handle_id = require_str(&resp, "handle_id");
    let completion_rx = register_completion_notifier(&handle_id);

    let mut cancel_req = dict();
    cancel_req.insert("handle_id".into(), vstr(&handle_id));
    let cancel_resp = require_dict(call("hostlib_tools_cancel_handle", cancel_req).unwrap());
    assert!(require_bool(&cancel_resp, "cancelled"));
    if let Some(rx) = completion_rx {
        let _ = rx.recv();
    }
}

#[test]
fn run_command_background_after_returns_progress_snapshot() {
    let _session_guard = harn_vm::agent_sessions::enter_current_session(unique_session_id(
        "test-run-command-background-after",
    ));
    let mut config = MockProcessConfig::running();
    config.stdout = b"started\n".to_vec();
    let (_spawner, _controller, _guard) = install_mock_with(config);

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("background_after_ms".into(), VmValue::Int(50));
    req.insert("progress_max_inline_bytes".into(), VmValue::Int(200));
    req.insert(
        "snapshot_binding".into(),
        VmValue::dict(snapshot_binding_fixture()),
    );
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    assert_eq!(require_str(&resp, "status"), "running");
    assert_eq!(require_str(&resp, "feedback_kind"), "tool_progress");
    assert_eq!(require_str(&resp, "stdout"), "started\n");
    assert!(require_str(&resp, "output_path").contains("harn-command-"));
    assert_snapshot_binding(&resp);
    let handle_id = require_str(&resp, "handle_id");

    let mut read_req = dict();
    read_req.insert("handle_id".into(), vstr(&handle_id));
    read_req.insert("length".into(), VmValue::Int(200));
    let read_resp = require_dict(call("hostlib_tools_read_command_output", read_req).unwrap());
    assert_eq!(require_str(&read_resp, "content"), "started\n");
    assert!(require_str(&read_resp, "path").contains("combined.txt"));
    assert_eq!(require_int(&read_resp, "total_bytes"), 8);

    let mut wait_req = dict();
    wait_req.insert("handle_id".into(), vstr(&handle_id));
    wait_req.insert("timeout_ms".into(), VmValue::Int(0));
    let waited = require_dict(call("hostlib_tools_wait_command", wait_req).unwrap());
    assert_eq!(require_str(&waited, "status"), "running");
    assert_eq!(require_str(&waited, "combined"), "started\n");
    assert_eq!(require_str(&waited, "inline_output"), "started\n");
    assert_eq!(require_int(&waited, "byte_count"), 8);
    assert_snapshot_binding(&waited);

    let completion_rx = register_completion_notifier(&handle_id);
    let mut cancel_req = dict();
    cancel_req.insert("handle_id".into(), vstr(&handle_id));
    let cancel_resp = require_dict(call("hostlib_tools_cancel_handle", cancel_req).unwrap());
    assert!(require_bool(&cancel_resp, "cancelled"));
    if let Some(rx) = completion_rx {
        let _ = rx.recv();
    }
}

// Vacuity guard for the whole background-command feature: a command that
// converts to background must ESCAPE the foreground `timeout_ms` kill. The same
// `force_timeout` + short `timeout_ms` shape that kills a blocking exec in
// `run_command_kills_child_when_timeout_elapses` must instead survive here,
// because `background_after_ms` routes to the background branch, which never
// applies `timeout_ms`. Without this, the whole mechanism is vacuous for the
// headline case (a longer-than-expected command dying at the timeout).
#[test]
fn run_command_background_after_survives_foreground_timeout() {
    let session_id = unique_session_id("test-run-command-background-survives-timeout");
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);
    let config = MockProcessConfig {
        force_timeout: true,
        ..MockProcessConfig::running()
    };
    let (_spawner, controller, _guard) = install_mock_with(config);

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "30"]));
    // The exact short foreground timeout that kills in the blocking-exec test...
    req.insert("timeout_ms".into(), VmValue::Int(150));
    // ...but background_after_ms hands back a running handle instead of a kill.
    req.insert("background_after_ms".into(), VmValue::Int(50));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    assert_eq!(
        require_str(&resp, "status"),
        "running",
        "background_after must return a running handle, not a timeout kill",
    );
    assert!(
        !controller.was_killed(),
        "a backgrounded command must NOT be killed by the foreground timeout_ms",
    );
    let handle_id = require_str(&resp, "handle_id");

    // The command completes well after the foreground timeout would have fired;
    // the handle drains a normal exit-0 result through the session inbox.
    let completion_rx =
        register_completion_notifier(&handle_id).expect("handle should still be live");
    controller.append_stdout(b"done-after-timeout\n");
    controller.complete_with(ExitStatus::from_code(0));
    completion_rx.recv().expect("waiter completion never fired");

    let mut wait_req = dict();
    wait_req.insert("handle_id".into(), vstr(&handle_id));
    wait_req.insert("timeout_ms".into(), VmValue::Int(0));
    let waited = require_dict(call("hostlib_tools_wait_command", wait_req).unwrap());
    assert_eq!(require_str(&waited, "status"), "completed");
    assert_eq!(require_int(&waited, "exit_code"), 0);
    assert!(
        require_str(&waited, "stdout").contains("done-after-timeout"),
        "waited result must carry the post-timeout output",
    );
}

// Command-ledger Phase 1: `list_handles` is the loop's liveness/reconciliation
// query. An auto-converted (`background_after_ms`) command is an `awaited` lease
// and is listed until it exits and its waiter drains it.
#[test]
fn list_handles_reports_live_awaited_handle_and_clears_on_exit() {
    let session_id = unique_session_id("test-list-handles-awaited");
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);
    let (_spawner, controller, _guard) = install_mock_with(MockProcessConfig::running());

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("background_after_ms".into(), VmValue::Int(50));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    let handle_id = require_str(&resp, "handle_id");

    let listed = require_dict(call("hostlib_tools_list_handles", dict()).unwrap());
    let handles = require_list(&listed, "handles");
    assert_eq!(handles.len(), 1, "the awaited handle must be listed");
    let row = as_dict(&handles[0]);
    assert_eq!(require_str(&row, "handle_id"), handle_id);
    assert_eq!(require_str(&row, "lease"), "awaited");
    assert!(!require_str(&row, "command_or_op_descriptor").is_empty());
    assert!(!require_str(&row, "started_at").is_empty());

    let completion_rx =
        register_completion_notifier(&handle_id).expect("handle should still be live");
    controller.append_stdout(b"done\n");
    controller.complete_with(ExitStatus::from_code(0));
    completion_rx.recv().expect("waiter completion never fired");

    let after = require_dict(call("hostlib_tools_list_handles", dict()).unwrap());
    assert_eq!(
        require_list(&after, "handles").len(),
        0,
        "a completed-and-drained handle must leave the live list",
    );
}

// A bare `background: true` (detach) with no inline window is the fire-and-forget
// service idiom -> `service` lease, distinguishable in `list_handles`.
#[test]
fn run_command_detach_registers_service_lease() {
    let session_id = unique_session_id("test-list-handles-service");
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);
    let (_spawner, _controller, _guard) = install_mock_with(MockProcessConfig::running());

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("background".into(), VmValue::Bool(true));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    let handle_id = require_str(&resp, "handle_id");

    let listed = require_dict(call("hostlib_tools_list_handles", dict()).unwrap());
    let handles = require_list(&listed, "handles");
    assert_eq!(handles.len(), 1);
    let row = as_dict(&handles[0]);
    assert_eq!(require_str(&row, "handle_id"), handle_id);
    assert_eq!(
        require_str(&row, "lease"),
        "service",
        "a bare detach is a fire-and-forget service lease",
    );

    let mut cancel = dict();
    cancel.insert("handle_id".into(), vstr(&handle_id));
    let _ = call("hostlib_tools_cancel_handle", cancel);
}

// The conversion snapshot seeds the loop's per-handle delta cursor and the
// stall / first-stderr decision triggers.
#[test]
fn background_after_snapshot_carries_delta_and_stall_fields() {
    let _session_guard = harn_vm::agent_sessions::enter_current_session(unique_session_id(
        "test-snapshot-delta-fields",
    ));
    let mut config = MockProcessConfig::running();
    config.stdout = b"building\n".to_vec();
    let (_spawner, _controller, _guard) = install_mock_with(config);

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["cargo", "build"]));
    req.insert("background_after_ms".into(), VmValue::Int(50));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    assert_eq!(require_str(&resp, "status"), "running");
    assert_eq!(
        require_int(&resp, "output_offset"),
        require_int(&resp, "byte_count"),
        "output_offset seeds the loop's delta cursor from the current byte count",
    );
    assert_eq!(require_int(&resp, "stderr_byte_count"), 0);
    assert!(require_int(&resp, "silence_ms") >= 0);
}

#[test]
fn run_command_background_after_requeues_unrelated_feedback_without_restamping() {
    let session_id = unique_session_id("test-run-command-background-after-requeue");
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);
    harn_vm::orchestration::agent_inbox::push(&session_id, "notice", "first", "test");
    harn_vm::orchestration::agent_inbox::push(&session_id, "notice", "second", "test");

    let (_spawner, _controller, _guard) = install_mock_with(MockProcessConfig::running());
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("background_after_ms".into(), VmValue::Int(50));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    assert_eq!(require_str(&resp, "status"), "running");
    let remaining = harn_vm::orchestration::agent_inbox::drain(&session_id);
    assert_eq!(remaining.len(), 2);
    assert_eq!(remaining[0].content, "first");
    assert_eq!(remaining[1].content, "second");
    assert_eq!(remaining[0].sequence, 1);
    assert_eq!(remaining[1].sequence, 2);
    let handle_id = require_str(&resp, "handle_id");
    let completion_rx = register_completion_notifier(&handle_id);

    let mut cancel_req = dict();
    cancel_req.insert("handle_id".into(), vstr(&handle_id));
    let cancel_resp = require_dict(call("hostlib_tools_cancel_handle", cancel_req).unwrap());
    assert!(require_bool(&cancel_resp, "cancelled"));
    if let Some(rx) = completion_rx {
        let _ = rx.recv();
    }
}

#[test]
fn run_command_long_running_feedback_fires_after_exit() {
    // Use a process-unique session id so parallel tests don't interfere.
    let session_id = format!(
        "test-lr-feedback-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    );

    // Stay running until the controller signals exit.
    let (_spawner, controller, _guard) = install_mock_with(MockProcessConfig::running());

    let info = harn_hostlib::tools::long_running::spawn_long_running(
        "test_builtin",
        "bash".into(),
        vec![
            "-c".into(),
            "echo 'hello stdout'; echo 'hello stderr' 1>&2".into(),
        ],
        None,
        std::collections::BTreeMap::new(),
        session_id.clone(),
    )
    .expect("spawn_long_running failed");
    assert!(!info.handle_id.is_empty());

    // Register a completion notifier BEFORE pushing the exit so we can
    // recv on it once the waiter publishes the feedback item.
    let completion_rx =
        register_completion_notifier(&info.handle_id).expect("handle should still be live");

    controller.append_stdout(b"hello stdout\n");
    controller.append_stderr(b"hello stderr\n");
    controller.complete_with(ExitStatus::from_code(0));

    completion_rx.recv().expect("waiter completion never fired");

    let items = harn_vm::orchestration::agent_inbox::drain(&session_id);
    assert_eq!(items.len(), 1, "expected exactly one feedback item");
    let entry = &items[0];
    assert_eq!(
        entry.kind, "tool_result",
        "unexpected feedback kind: {}",
        entry.kind
    );
    let payload: serde_json::Value =
        serde_json::from_str(&entry.content).expect("feedback content not valid JSON");
    assert_eq!(
        payload["handle_id"].as_str().unwrap(),
        info.handle_id,
        "handle_id mismatch in feedback"
    );
    assert_eq!(payload["exit_code"], 0);
    assert_eq!(payload["status"], "completed");
    assert!(payload["output_path"]
        .as_str()
        .unwrap()
        .contains("combined.txt"));
    assert!(
        payload["stdout"].as_str().unwrap().contains("hello stdout"),
        "stdout missing: {}",
        payload["stdout"]
    );
    assert!(
        payload["stderr"].as_str().unwrap().contains("hello stderr"),
        "stderr missing: {}",
        payload["stderr"]
    );
    assert!(
        payload["duration_ms"].as_i64().unwrap() >= 0,
        "duration_ms must be non-negative"
    );
}

#[test]
fn wait_command_returns_completed_background_result() {
    let session_id = unique_session_id("test-wait-command-completed");
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);
    let (_spawner, controller, _guard) = install_mock_with(MockProcessConfig::running());

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sh", "-c", "echo done"]));
    req.insert("background".into(), VmValue::Bool(true));
    let start = require_dict(call("hostlib_tools_run_command", req).unwrap());
    let handle_id = require_str(&start, "handle_id");
    let completion_rx =
        register_completion_notifier(&handle_id).expect("handle should still be live");

    controller.append_stdout(b"done\n");
    controller.complete_with(ExitStatus::from_code(0));
    completion_rx.recv().expect("waiter completion never fired");

    let mut wait_req = dict();
    wait_req.insert("handle_id".into(), vstr(&handle_id));
    let waited = require_dict(call("hostlib_tools_wait_command", wait_req).unwrap());

    assert_eq!(require_str(&waited, "status"), "completed");
    assert_eq!(require_str(&waited, "feedback_kind"), "tool_result");
    assert_eq!(require_str(&waited, "handle_id"), handle_id);
    assert_eq!(require_int(&waited, "exit_code"), 0);
    assert_eq!(require_str(&waited, "stdout"), "done\n");
    assert!(!require_bool(&waited, "timed_out"));
}

#[test]
fn wait_command_carries_background_snapshot_binding_to_completion() {
    let session_id = unique_session_id("test-wait-command-snapshot-binding");
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);
    let (_spawner, controller, _guard) = install_mock_with(MockProcessConfig::running());

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sh", "-c", "echo done"]));
    req.insert("background".into(), VmValue::Bool(true));
    req.insert(
        "snapshot_binding".into(),
        VmValue::dict(snapshot_binding_fixture()),
    );
    let start = require_dict(call("hostlib_tools_run_command", req).unwrap());
    assert_snapshot_binding(&start);
    let handle_id = require_str(&start, "handle_id");

    let mut poll_req = dict();
    poll_req.insert("handle_id".into(), vstr(&handle_id));
    poll_req.insert("timeout_ms".into(), VmValue::Int(0));
    let running = require_dict(call("hostlib_tools_wait_command", poll_req).unwrap());
    assert_eq!(require_str(&running, "status"), "running");
    assert_snapshot_binding(&running);

    let completion_rx =
        register_completion_notifier(&handle_id).expect("handle should still be live");
    controller.append_stdout(b"done\n");
    controller.complete_with(ExitStatus::from_code(0));
    completion_rx.recv().expect("waiter completion never fired");

    let mut wait_req = dict();
    wait_req.insert("handle_id".into(), vstr(&handle_id));
    let waited = require_dict(call("hostlib_tools_wait_command", wait_req).unwrap());

    assert_eq!(require_str(&waited, "status"), "completed");
    assert_eq!(require_str(&waited, "feedback_kind"), "tool_result");
    assert_eq!(require_str(&waited, "handle_id"), handle_id);
    assert_eq!(require_str(&waited, "stdout"), "done\n");
    assert_snapshot_binding(&waited);
}

#[test]
fn wait_command_reports_running_when_handle_has_not_completed() {
    let session_id = unique_session_id("test-wait-command-running");
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);
    let (_spawner, _controller, _guard) = install_mock_with(MockProcessConfig::running());

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("background".into(), VmValue::Bool(true));
    let start = require_dict(call("hostlib_tools_run_command", req).unwrap());
    let handle_id = require_str(&start, "handle_id");
    let completion_rx = register_completion_notifier(&handle_id);

    let mut wait_req = dict();
    wait_req.insert("handle_id".into(), vstr(&handle_id));
    wait_req.insert("timeout_ms".into(), VmValue::Int(0));
    let waited = require_dict(call("hostlib_tools_wait_command", wait_req).unwrap());

    assert_eq!(require_str(&waited, "status"), "running");
    assert_eq!(require_str(&waited, "handle_id"), handle_id);
    assert!(!require_bool(&waited, "completed"));
    assert!(require_str(&waited, "output_path").contains("combined.txt"));
    assert_eq!(require_int(&waited, "byte_count"), 0);
    assert_eq!(require_int(&waited, "line_count"), 0);

    let mut cancel_req = dict();
    cancel_req.insert("handle_id".into(), vstr(&handle_id));
    let cancel_resp = require_dict(call("hostlib_tools_cancel_handle", cancel_req).unwrap());
    assert!(require_bool(&cancel_resp, "cancelled"));
    if let Some(rx) = completion_rx {
        let _ = rx.recv();
    }
}

#[test]
fn wait_command_requeues_unrelated_feedback() {
    let session_id = unique_session_id("test-wait-command-requeue");
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);
    harn_vm::orchestration::agent_inbox::push(&session_id, "notice", "keep me", "test");

    let mut wait_req = dict();
    wait_req.insert("handle_id".into(), vstr("hto-missing"));
    wait_req.insert("timeout_ms".into(), VmValue::Int(0));
    let waited = require_dict(call("hostlib_tools_wait_command", wait_req).unwrap());

    assert_eq!(require_str(&waited, "status"), "running");
    let remaining = harn_vm::orchestration::agent_inbox::drain(&session_id);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].kind, "notice");
    assert_eq!(remaining[0].content, "keep me");
}

#[test]
fn cancel_handle_kills_long_running_process() {
    let session_id = format!(
        "test-lr-cancel-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    );

    let (_spawner, _controller, _guard) = install_mock_with(MockProcessConfig::running());

    let info = harn_hostlib::tools::long_running::spawn_long_running(
        "test_builtin",
        "sleep".into(),
        vec!["30".into()],
        None,
        std::collections::BTreeMap::new(),
        session_id.clone(),
    )
    .expect("spawn_long_running failed");

    // Register so cancel signals it.
    let completion_rx = register_completion_notifier(&info.handle_id);

    // Cancel via the builtin — should return cancelled: true.
    let mut req = dict();
    req.insert("handle_id".into(), vstr(&info.handle_id));
    let resp = require_dict(call("hostlib_tools_cancel_handle", req).unwrap());
    assert!(require_bool(&resp, "cancelled"));
    assert_eq!(require_str(&resp, "handle_id"), info.handle_id);

    // Cancelling the same handle a second time should return cancelled: false.
    let mut req2 = dict();
    req2.insert("handle_id".into(), vstr(&info.handle_id));
    let resp2 = require_dict(call("hostlib_tools_cancel_handle", req2).unwrap());
    assert!(
        !require_bool(&resp2, "cancelled"),
        "second cancel should return false"
    );

    if let Some(rx) = completion_rx {
        let _ = rx.recv();
    }

    // Cancelled handles never push feedback — drain returns empty.
    let items = harn_vm::orchestration::agent_inbox::drain(&session_id);
    assert!(
        items.is_empty(),
        "cancelled handle should not push feedback, got {} entries",
        items.len()
    );
}

#[test]
fn wait_command_projects_spawn_time_sandbox_assessment() {
    let workspace = tempdir().unwrap();
    let _policy = install_confining_policy(workspace.path());
    let session_id = unique_session_id("test-wait-command-sandbox-denial");
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);
    let (_spawner, controller, _guard) = install_mock_with(MockProcessConfig::running());

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["fixture", "nested-child"]));
    req.insert(
        "cwd".into(),
        vstr(workspace.path().to_string_lossy().as_ref()),
    );
    req.insert("background".into(), VmValue::Bool(true));
    let start = require_dict(call("hostlib_tools_run_command", req).unwrap());
    let handle_id = require_str(&start, "handle_id");
    let completion_rx =
        register_completion_notifier(&handle_id).expect("handle should still be live");

    controller.append_stderr(b"nested child: Operation not permitted\n");
    controller.complete_with(ExitStatus::from_code(1));
    completion_rx.recv().expect("waiter completion never fired");

    let mut wait_req = dict();
    wait_req.insert("handle_id".into(), vstr(&handle_id));
    let waited_value = call("hostlib_tools_wait_command", wait_req).unwrap();
    assert_response_matches_schema("wait_command", &waited_value);
    let waited = require_dict(waited_value);
    let sandbox = require_nested_dict(&waited, "sandbox");
    assert_eq!(require_str(&sandbox, "denial_reporting"), "inferred_only");
    let denial = require_nested_dict(&waited, "denial");
    assert_eq!(require_str(&denial, "gate"), "process_sandbox");
    assert_eq!(require_str(&denial, "operation"), "unknown");
    require_nil(&denial, "resource");
    assert_eq!(require_list(&denial, "command").len(), 2);
    assert_eq!(
        require_str(&denial, "stderr_excerpt"),
        "nested child: Operation not permitted\n"
    );
}

#[test]
fn cancel_handle_can_wait_for_timed_out_result() {
    let session_id = unique_session_id("test-lr-cancel-wait-result");
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);

    let mut config = MockProcessConfig::running();
    config.stdout = b"before timeout\n".to_vec();
    config.cleanup_report = Some(cleanup_report_fixture(9));
    let (_spawner, _controller, _guard) = install_mock_with(config);

    let mut start_req = dict();
    start_req.insert("argv".into(), vlist_str(&["sleep", "30"]));
    start_req.insert("background".into(), VmValue::Bool(true));
    start_req.insert("capture".into(), {
        let mut capture: BTreeMap<String, VmValue> = BTreeMap::new();
        capture.insert("max_inline_bytes".into(), VmValue::Int(200));
        VmValue::dict(capture)
    });
    let start = require_dict(call("hostlib_tools_run_command", start_req).unwrap());
    let handle_id = require_str(&start, "handle_id");

    let completion_rx =
        register_completion_notifier(&handle_id).expect("handle should still be live");
    let cancel_handle_id = handle_id.clone();
    let cancel_thread = std::thread::spawn(move || {
        let mut cancel_req = dict();
        cancel_req.insert("handle_id".into(), vstr(&cancel_handle_id));
        cancel_req.insert("wait_result_ms".into(), VmValue::Int(60_000));
        cancel_req.insert("timed_out".into(), VmValue::Bool(true));
        let cancel = require_dict(call("hostlib_tools_cancel_handle", cancel_req).unwrap());

        assert!(require_bool(&cancel, "cancelled"));
        let result = require_nested_dict(&cancel, "result");
        assert_response_matches_schema("wait_command", &VmValue::dict(result.clone()));
        (
            require_str(&cancel, "handle_id"),
            require_str(&result, "handle_id"),
            require_str(&result, "status"),
            require_bool(&result, "timed_out"),
            require_int(&result, "exit_code"),
            require_str(&result, "stdout"),
            require_str(&result, "output_path"),
            require_str(&result, "stdout_path"),
            require_int(&result, "byte_count"),
            require_int(
                &require_nested_dict(&result, "process_cleanup"),
                "reaped_child_count",
            ),
        )
    });
    completion_rx.recv().expect("waiter completion never fired");
    let (
        cancelled_handle_id,
        result_handle_id,
        status,
        timed_out,
        exit_code,
        stdout,
        output_path,
        stdout_path,
        byte_count,
        reaped_child_count,
    ) = cancel_thread.join().expect("cancel thread panicked");

    assert_eq!(cancelled_handle_id, handle_id);
    assert_eq!(result_handle_id, handle_id);
    assert_eq!(status, "timed_out");
    assert!(timed_out);
    assert_eq!(exit_code, -1);
    assert_eq!(stdout, "before timeout\n");
    assert!(output_path.contains("combined.txt"));
    assert_eq!(
        std::fs::read_to_string(stdout_path).unwrap(),
        "before timeout\n"
    );
    assert!(byte_count >= 15);
    assert_eq!(reaped_child_count, 1);

    let items = harn_vm::orchestration::agent_inbox::drain(&session_id);
    assert!(
        items.is_empty(),
        "cancel result should not also enqueue feedback, got {} entries",
        items.len()
    );
}

#[test]
fn cancel_handle_unknown_handle_returns_false() {
    let mut req = dict();
    req.insert("handle_id".into(), vstr("hto-deadbeef-no-such-handle"));
    let resp = require_dict(call("hostlib_tools_cancel_handle", req).unwrap());
    assert!(!require_bool(&resp, "cancelled"));
}

#[test]
fn run_test_background_returns_handle() {
    let _session_guard = harn_vm::agent_sessions::enter_current_session(unique_session_id(
        "test-run-test-long-running",
    ));
    let (_spawner, _controller, _guard) = install_mock_with(MockProcessConfig::running());

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("background".into(), VmValue::Bool(true));
    let resp = require_dict(call("hostlib_tools_run_test", req).unwrap());
    let handle_id = require_str(&resp, "handle_id");
    assert!(
        handle_id.starts_with("hto-"),
        "unexpected handle_id: {handle_id}"
    );

    let completion_rx = register_completion_notifier(&handle_id);
    let mut cancel_req = dict();
    cancel_req.insert("handle_id".into(), vstr(&handle_id));
    call("hostlib_tools_cancel_handle", cancel_req).unwrap();
    if let Some(rx) = completion_rx {
        let _ = rx.recv();
    }
}

#[test]
fn run_build_command_background_returns_handle() {
    let _session_guard = harn_vm::agent_sessions::enter_current_session(unique_session_id(
        "test-run-build-long-running",
    ));
    let (_spawner, _controller, _guard) = install_mock_with(MockProcessConfig::running());

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("background".into(), VmValue::Bool(true));
    let resp = require_dict(call("hostlib_tools_run_build_command", req).unwrap());
    let handle_id = require_str(&resp, "handle_id");
    assert!(
        handle_id.starts_with("hto-"),
        "unexpected handle_id: {handle_id}"
    );

    let completion_rx = register_completion_notifier(&handle_id);
    let mut cancel_req = dict();
    cancel_req.insert("handle_id".into(), vstr(&handle_id));
    call("hostlib_tools_cancel_handle", cancel_req).unwrap();
    if let Some(rx) = completion_rx {
        let _ = rx.recv();
    }
}

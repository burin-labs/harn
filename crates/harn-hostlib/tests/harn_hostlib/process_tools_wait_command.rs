//! Focused mock coverage for `wait_command` direct result synchronization.

#![cfg(unix)]

use harn_vm::value::VmDictExt as _;
use std::sync::mpsc;

use harn_hostlib::process::{ExitStatus, MockProcessConfig};
use harn_hostlib::tools::long_running::register_result_notifier;
use harn_vm::VmValue;

use super::support::*;

#[test]
fn wait_command_waits_for_live_handle_result_and_consumes_feedback() {
    let session_id = unique_session_id("test-wait-command-direct-result");
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);
    let (_spawner, controller, _guard) = install_mock_with(MockProcessConfig::running());

    let mut req = dict_map();
    req.insert("argv".into(), vlist_str(&["sh", "-c", "echo direct"]));
    req.put_bool("background", true);
    let start = require_dict(call_tool("hostlib_tools_run_command", req).unwrap());
    let handle_id = require_str(&start, "handle_id");
    let cwd = require_str(&start, "cwd");

    let wait_handle_id = handle_id.clone();
    let wait_session_id = session_id.clone();
    let waiter = std::thread::spawn(move || {
        let _session_guard = harn_vm::agent_sessions::enter_current_session(wait_session_id);
        let mut wait_req = dict_map();
        wait_req.insert("handle_id".into(), vm_string(&wait_handle_id));
        wait_req.put_int("timeout_ms", 5_000);
        require_dict(call_tool("hostlib_tools_wait_command", wait_req).unwrap())
    });

    controller.append_stdout(b"direct\n");
    controller.complete_with(ExitStatus::from_code(0));

    let waited = waiter.join().expect("waiter thread panicked");
    assert_eq!(require_str(&waited, "status"), "completed");
    assert_eq!(require_str(&waited, "feedback_kind"), "tool_result");
    assert_eq!(require_str(&waited, "handle_id"), handle_id);
    assert_eq!(require_str(&waited, "cwd"), cwd);
    assert_eq!(require_str(&waited, "stdout"), "direct\n");

    let leftover = harn_vm::orchestration::agent_inbox::drain(&session_id);
    assert!(
        leftover.is_empty(),
        "explicit wait must consume matching tool_result feedback, got {leftover:?}"
    );

    // Terminal state is monotonic: consuming the one normal inbox delivery
    // must not make a later observation regress to `running`.
    let mut replay_req = dict_map();
    replay_req.insert("handle_id".into(), vm_string(&handle_id));
    let replayed = require_dict(call_tool("hostlib_tools_wait_command", replay_req).unwrap());
    assert_eq!(require_str(&replayed, "status"), "completed");
    assert_eq!(require_str(&replayed, "feedback_kind"), "tool_result");
    assert_eq!(require_str(&replayed, "handle_id"), handle_id);
    assert_eq!(require_str(&replayed, "stdout"), "direct\n");
    assert!(
        harn_vm::orchestration::agent_inbox::drain(&session_id).is_empty(),
        "replay must not manufacture a second inbox delivery"
    );

    // Cancellation observes the same immutable receipt rather than
    // ambiguously reporting that the handle might still be running.
    let mut cancel_req = dict_map();
    cancel_req.insert("handle_id".into(), vm_string(&handle_id));
    let cancel = require_dict(call_tool("hostlib_tools_cancel_handle", cancel_req).unwrap());
    assert!(matches!(
        cancel.get("cancelled"),
        Some(VmValue::Bool(false))
    ));
    let terminal = match cancel.get("result") {
        Some(VmValue::Dict(result)) => (**result).clone(),
        other => panic!("completed cancellation must replay its receipt, got {other:?}"),
    };
    assert_eq!(require_str(&terminal, "status"), "completed");
    assert_eq!(require_str(&terminal, "handle_id"), handle_id);
}

#[test]
fn background_finalization_reuses_active_artifact_lease() {
    let session_id = unique_session_id("test-background-artifact-lease");
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);
    let (_spawner, controller, _guard) = install_mock_with(MockProcessConfig::running());

    let mut req = dict_map();
    req.insert("argv".into(), vlist_str(&["sh", "-c", "echo leased"]));
    req.put_bool("background", true);
    let start = require_dict(call_tool("hostlib_tools_run_command", req).unwrap());
    let command_id = require_str(&start, "command_id");
    let handle_id = require_str(&start, "handle_id");

    let (wait_tx, wait_rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id);
        let mut wait_req = dict_map();
        wait_req.insert("handle_id".into(), vm_string(&handle_id));
        wait_req.put_int("timeout_ms", 5_000);
        let waited = require_dict(call_tool("hostlib_tools_wait_command", wait_req).unwrap());
        wait_tx.send(require_str(&waited, "status")).unwrap();
    });

    controller.append_stdout(b"leased\n");
    controller.complete_with(ExitStatus::from_code(0));

    assert_eq!(
        harn_clock::test_support::recv_within(
            "wait_command status (a hang means background finalization deadlocked on its \
             active artifact lease)",
            &wait_rx,
        ),
        "completed"
    );
    waiter.join().expect("background waiter thread panicked");

    let mut read_req = dict_map();
    read_req.insert("command_id".into(), vm_string(&command_id));
    let output = require_dict(call_tool("hostlib_tools_read_command_output", read_req).unwrap());
    assert_eq!(require_str(&output, "content"), "leased\n");
}

#[test]
fn long_running_result_notifier_fires_after_inbox_feedback_is_visible() {
    let session_id = unique_session_id("test-long-running-result-notifier-order");
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);
    let (_spawner, controller, _guard) = install_mock_with(MockProcessConfig::running());

    let mut req = dict_map();
    req.insert("argv".into(), vlist_str(&["sh", "-c", "echo ordered"]));
    req.put_bool("background", true);
    let start = require_dict(call_tool("hostlib_tools_run_command", req).unwrap());
    let handle_id = require_str(&start, "handle_id");
    let result_rx = register_result_notifier(&handle_id).expect("handle should still be live");

    controller.append_stdout(b"ordered\n");
    controller.complete_with(ExitStatus::from_code(0));

    let result = require_dict(result_rx.recv().expect("result notifier never fired"));
    assert_eq!(require_str(&result, "status"), "completed");
    assert_eq!(require_str(&result, "handle_id"), handle_id);
    assert_eq!(require_str(&result, "stdout"), "ordered\n");

    let inbox = harn_vm::orchestration::agent_inbox::drain(&session_id);
    assert_eq!(
        inbox.len(),
        1,
        "result notifier must fire after inbox publish"
    );
    assert_eq!(inbox[0].kind, "tool_result");
    let payload: serde_json::Value = serde_json::from_str(&inbox[0].content).expect("json");
    assert_eq!(
        payload.get("handle_id").and_then(|value| value.as_str()),
        Some(handle_id.as_str())
    );
}

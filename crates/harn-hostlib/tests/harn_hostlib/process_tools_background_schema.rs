#![cfg(unix)]

use std::sync::Arc;

use harn_hostlib::process::{install_spawner, ExitStatus, MockProcessConfig, MockSpawner};
use harn_hostlib::tools::long_running::register_completion_notifier;
use harn_vm::VmValue;

use super::support::*;

fn cancel_handle(handle_id: &str) {
    let completion_rx = register_completion_notifier(handle_id);
    let mut cancel_req = dict_map();
    cancel_req.insert("handle_id".into(), vm_string(handle_id));
    let cancel_resp = require_dict(call_tool("hostlib_tools_cancel_handle", cancel_req).unwrap());
    assert!(require_bool(&cancel_resp, "cancelled"));
    if let Some(rx) = completion_rx {
        let _ = rx.recv();
    }
}

#[test]
fn run_command_background_after_snapshot_satisfies_response_schema() {
    let _session_guard = harn_vm::agent_sessions::enter_current_session(unique_session_id(
        "test-run-command-background-after-snapshot-schema",
    ));
    let mut config = MockProcessConfig::running();
    config.stdout = b"started\n".to_vec();
    let (_spawner, _controller, _guard) = install_mock_with(config);

    let mut req = dict_map();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("background_after_ms".into(), VmValue::Int(50));
    req.insert("progress_max_inline_bytes".into(), VmValue::Int(200));
    req.insert(
        "snapshot_binding".into(),
        VmValue::dict(snapshot_binding_fixture()),
    );
    let resp_value = call_tool("hostlib_tools_run_command", req).unwrap();
    assert_response_matches_schema("run_command", &resp_value);

    let resp = require_dict(resp_value);
    assert_eq!(require_str(&resp, "status"), "running");
    assert_eq!(require_str(&resp, "feedback_kind"), "tool_progress");
    assert!(!require_str(&resp, "cwd").is_empty());
    assert_snapshot_binding(&resp);
    cancel_handle(&require_str(&resp, "handle_id"));
}

#[test]
fn run_command_background_after_waits_past_progress_for_terminal_result() {
    let session_id = unique_session_id("test-run-command-background-after-terminal");
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);

    let spawner = Arc::new(MockSpawner::new());
    let controller = spawner.enqueue(MockProcessConfig::running());
    let _guard = install_spawner(spawner);

    harn_vm::orchestration::agent_inbox::push(&session_id, "notice", "first", "test");
    harn_vm::orchestration::agent_inbox::push(&session_id, "notice", "second", "test");
    let (driver_ready_tx, driver_ready_rx) = std::sync::mpsc::sync_channel::<()>(0);

    // Complete only after the progress publisher has fired. This is the exact
    // ordering that used to make progress_interval_ms shorten the independent
    // background_after_ms inline window. Waiting on the inbox notification is
    // event-driven; the test contains no timing sleep.
    let completion_session_id = session_id.clone();
    let completer = std::thread::spawn(move || {
        let foreign = harn_vm::orchestration::agent_inbox::drain(&completion_session_id);
        assert_eq!(foreign.len(), 2);
        assert_eq!(foreign[0].content, "first");
        assert_eq!(foreign[0].sequence, 1);
        assert_eq!(foreign[1].content, "second");
        assert_eq!(foreign[1].sequence, 2);
        driver_ready_tx
            .send(())
            .expect("driver-ready receiver dropped");
        assert!(
            harn_vm::orchestration::agent_inbox::wait_sync(
                &completion_session_id,
                std::time::Duration::from_secs(5),
            ),
            "progress feedback was never published",
        );
        for entry in foreign.into_iter().rev() {
            harn_vm::orchestration::agent_inbox::requeue_front(entry);
        }
        controller.append_stdout(b"verified\n");
        controller.complete_with(ExitStatus::from_code(0));
    });
    driver_ready_rx
        .recv()
        .expect("completion driver did not start");

    let mut req = dict_map();
    req.insert("argv".into(), vlist_str(&["verify"]));
    req.insert("background_after_ms".into(), VmValue::Int(5_000));
    req.insert("progress_interval_ms".into(), VmValue::Int(1));
    req.insert(
        "snapshot_binding".into(),
        VmValue::dict(snapshot_binding_fixture()),
    );
    let resp_value = call_tool("hostlib_tools_run_command", req).unwrap();
    completer.join().expect("completion driver panicked");

    assert_response_matches_schema("run_command", &resp_value);
    let resp = require_dict(resp_value);
    assert_eq!(require_str(&resp, "status"), "completed");
    assert_eq!(require_str(&resp, "feedback_kind"), "tool_result");
    assert_eq!(require_int(&resp, "exit_code"), 0);
    assert_eq!(require_str(&resp, "stdout"), "verified\n");
    assert!(
        !require_str(&resp, "handle_id").is_empty(),
        "inline completion must retain its continuation handle",
    );
    assert_snapshot_binding(&resp);

    let foreign = harn_vm::orchestration::agent_inbox::drain(&session_id);
    assert_eq!(foreign.len(), 2);
    assert_eq!(foreign[0].content, "first");
    assert_eq!(foreign[0].sequence, 1);
    assert_eq!(foreign[1].content, "second");
    assert_eq!(foreign[1].sequence, 2);
}

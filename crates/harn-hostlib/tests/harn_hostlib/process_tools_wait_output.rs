//! Deterministic, in-process background output-wait coverage.

#![cfg(unix)]

use harn_vm::value::VmDictExt as _;
use std::sync::Arc;
use std::time::Duration;

use harn_hostlib::process::{
    install_spawner, ExitStatus, MockHandleController, MockProcessConfig, MockSpawner, SpawnerGuard,
};
use harn_hostlib::tools::long_running::register_completion_notifier;
use harn_hostlib::HostlibError;
use harn_vm::VmValue;

use super::support::*;

async fn call_async(
    builtin: &str,
    request: harn_vm::value::DictMap,
) -> Result<VmValue, HostlibError> {
    let registry = tools_registry();
    let entry = registry
        .find_async(builtin)
        .expect("async builtin registered");
    (entry.handler)(vec![VmValue::dict(request)]).await
}

fn install_running() -> (MockHandleController, SpawnerGuard) {
    let spawner = Arc::new(MockSpawner::new());
    let controller = spawner.enqueue(MockProcessConfig::running());
    let guard = install_spawner(spawner);
    (controller, guard)
}

fn start_background() -> harn_vm::value::DictMap {
    let mut request = dict_map();
    request.insert("argv".into(), vlist_str(&["mock-service"]));
    request.put_bool("background", true);
    require_dict(call_tool("hostlib_tools_run_command", request).unwrap())
}

fn wait_request(handle_id: &str, pattern: &str) -> harn_vm::value::DictMap {
    let mut request = dict_map();
    request.insert("handle_id".into(), vm_string(handle_id));
    request.insert("pattern".into(), vm_string(pattern));
    request
}

fn finish(controller: &MockHandleController, handle_id: &str) {
    let completion = register_completion_notifier(handle_id).expect("live handle");
    controller.complete_with(ExitStatus::from_code(0));
    completion.recv().expect("background finalization");
}

#[tokio::test(flavor = "current_thread")]
async fn matches_split_literal_and_preserves_offsets() {
    let (controller, _guard) = install_running();
    let started = start_background();
    let handle_id = require_str(&started, "handle_id");
    let cwd = require_str(&started, "cwd");
    controller.append_stdout(b"prefix re");
    tokio::task::yield_now().await;
    controller.append_stdout(b"ady suffix");

    let event = require_dict(
        call_async(
            "hostlib_tools_wait_command_output",
            wait_request(&handle_id, "ready"),
        )
        .await
        .unwrap(),
    );
    assert_eq!(require_str(&event, "status"), "matched");
    assert!(require_bool(&event, "matched"));
    assert_eq!(require_int(&event, "match_start"), 7);
    assert_eq!(require_int(&event, "match_end"), 12);
    assert_eq!(require_int(&event, "next_offset"), 12);
    let result = event
        .get("result")
        .and_then(VmValue::as_dict)
        .expect("matched event result");
    assert_eq!(require_str(result, "cwd"), cwd);
    finish(&controller, &handle_id);
}

#[tokio::test(flavor = "current_thread")]
async fn selects_stderr_and_matches_split_regex() {
    let (controller, _guard) = install_running();
    let started = start_background();
    let handle_id = require_str(&started, "handle_id");
    controller.append_stdout(b"ready on 1");
    controller.append_stderr(b"ready on");
    tokio::task::yield_now().await;
    controller.append_stderr(b" 4312");

    let mut request = wait_request(&handle_id, r"ready\s+on\s+\d+");
    request.insert("source".into(), vm_string("stderr"));
    request.put_bool("regex", true);
    let event = require_dict(
        call_async("hostlib_tools_wait_command_output", request)
            .await
            .unwrap(),
    );
    assert_eq!(require_str(&event, "match"), "ready on 4");
    assert_eq!(require_int(&event, "match_start"), 0);
    finish(&controller, &handle_id);
}

#[tokio::test(flavor = "current_thread")]
async fn reports_exit_before_unmatched_output() {
    let (controller, _guard) = install_running();
    let started = start_background();
    let handle_id = require_str(&started, "handle_id");
    let completion = register_completion_notifier(&handle_id).expect("live handle");
    controller.append_stdout(b"not yet");
    controller.complete_with(ExitStatus::from_code(7));
    completion.recv().expect("background finalization");

    let event = require_dict(
        call_async(
            "hostlib_tools_wait_command_output",
            wait_request(&handle_id, "ready"),
        )
        .await
        .unwrap(),
    );
    assert_eq!(require_str(&event, "status"), "exited");
    assert!(!require_bool(&event, "matched"));
    let result = require_dict(event.get("result").cloned().expect("result"));
    assert_eq!(require_str(&result, "status"), "completed");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn timeout_uses_paused_time() {
    let (controller, _guard) = install_running();
    let started = start_background();
    let handle_id = require_str(&started, "handle_id");
    let mut request = wait_request(&handle_id, "ready");
    request.put_int("timeout_ms", 1_000);
    let waiter =
        tokio::spawn(async move { call_async("hostlib_tools_wait_command_output", request).await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    let event = require_dict(waiter.await.unwrap().unwrap());
    assert_eq!(require_str(&event, "status"), "timed_out");
    assert!(require_bool(&event, "timed_out"));
    finish(&controller, &handle_id);
}

#[tokio::test(flavor = "current_thread")]
async fn caller_cancellation_drops_wait_without_touching_process() {
    let (controller, _guard) = install_running();
    let started = start_background();
    let handle_id = require_str(&started, "handle_id");
    let request = wait_request(&handle_id, "ready");
    let waiter =
        tokio::spawn(async move { call_async("hostlib_tools_wait_command_output", request).await });
    tokio::task::yield_now().await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert!(!controller.was_killed());
    finish(&controller, &handle_id);
}

#[tokio::test(flavor = "current_thread")]
async fn match_wins_when_output_and_exit_are_both_visible() {
    let (controller, _guard) = install_running();
    let started = start_background();
    let handle_id = require_str(&started, "handle_id");
    let completion = register_completion_notifier(&handle_id).expect("live handle");
    let request = wait_request(&handle_id, "ready");
    let waiter =
        tokio::spawn(async move { call_async("hostlib_tools_wait_command_output", request).await });
    tokio::task::yield_now().await;
    controller.append_stdout(b"ready");
    controller.complete_with(ExitStatus::from_code(0));
    completion.recv().expect("background finalization");
    let event = require_dict(waiter.await.unwrap().unwrap());
    assert_eq!(require_str(&event, "status"), "matched");
}

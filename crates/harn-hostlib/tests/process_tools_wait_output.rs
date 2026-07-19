//! Deterministic, in-process background output-wait coverage.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use harn_hostlib::process::{
    install_spawner, ExitStatus, MockHandleController, MockProcessConfig, MockSpawner, SpawnerGuard,
};
use harn_hostlib::tools::long_running::register_completion_notifier;
use harn_hostlib::tools::ToolsCapability;
use harn_hostlib::{BuiltinRegistry, HostlibCapability, HostlibError};
use harn_vm::VmValue;

fn registry() -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
    registry
}

fn call(builtin: &str, request: harn_vm::value::DictMap) -> Result<VmValue, HostlibError> {
    harn_hostlib::tools::permissions::enable_for_test();
    let registry = registry();
    let entry = registry.find(builtin).expect("sync builtin registered");
    (entry.handler)(&[VmValue::dict(request)])
}

async fn call_async(
    builtin: &str,
    request: harn_vm::value::DictMap,
) -> Result<VmValue, HostlibError> {
    harn_hostlib::tools::permissions::enable_for_test();
    let registry = registry();
    let entry = registry
        .find_async(builtin)
        .expect("async builtin registered");
    (entry.handler)(vec![VmValue::dict(request)]).await
}

fn dict() -> harn_vm::value::DictMap {
    harn_vm::value::DictMap::new()
}

fn vstr(value: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(value))
}

fn vlist(values: &[&str]) -> VmValue {
    VmValue::List(Arc::new(values.iter().map(|value| vstr(value)).collect()))
}

fn require_dict(value: VmValue) -> harn_vm::value::DictMap {
    match value {
        VmValue::Dict(map) => (*map).clone(),
        other => panic!("expected dict, got {other:?}"),
    }
}

fn string(map: &harn_vm::value::DictMap, key: &str) -> String {
    match map.get(key) {
        Some(VmValue::String(value)) => value.to_string(),
        other => panic!("expected string at {key}, got {other:?}"),
    }
}

fn int(map: &harn_vm::value::DictMap, key: &str) -> i64 {
    match map.get(key) {
        Some(VmValue::Int(value)) => *value,
        other => panic!("expected int at {key}, got {other:?}"),
    }
}

fn boolean(map: &harn_vm::value::DictMap, key: &str) -> bool {
    match map.get(key) {
        Some(VmValue::Bool(value)) => *value,
        other => panic!("expected bool at {key}, got {other:?}"),
    }
}

fn install_running() -> (MockHandleController, SpawnerGuard) {
    let spawner = Arc::new(MockSpawner::new());
    let controller = spawner.enqueue(MockProcessConfig::running());
    let guard = install_spawner(spawner);
    (controller, guard)
}

fn start_background() -> harn_vm::value::DictMap {
    let mut request = dict();
    request.insert("argv".into(), vlist(&["mock-service"]));
    request.insert("background".into(), VmValue::Bool(true));
    require_dict(call("hostlib_tools_run_command", request).unwrap())
}

fn wait_request(handle_id: &str, pattern: &str) -> harn_vm::value::DictMap {
    let mut request = dict();
    request.insert("handle_id".into(), vstr(handle_id));
    request.insert("pattern".into(), vstr(pattern));
    request
}

fn finish(controller: &MockHandleController, handle_id: &str) {
    let completion = register_completion_notifier(handle_id).expect("live handle");
    controller.complete_with(ExitStatus::from_code(0));
    completion.recv().expect("background finalization");
}

#[tokio::test(flavor = "current_thread")]
async fn output_wait_requires_deterministic_tools_gate() {
    harn_hostlib::tools::permissions::reset();
    let registry = registry();
    let entry = registry
        .find_async("hostlib_tools_wait_command_output")
        .expect("async builtin registered");
    let error = (entry.handler)(vec![VmValue::dict(dict())])
        .await
        .expect_err("disabled capability must fail before request handling");
    assert!(matches!(error, HostlibError::Backend { .. }));
}

#[tokio::test(flavor = "current_thread")]
async fn matches_split_literal_and_preserves_offsets() {
    let (controller, _guard) = install_running();
    let started = start_background();
    let handle_id = string(&started, "handle_id");
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
    assert_eq!(string(&event, "status"), "matched");
    assert!(boolean(&event, "matched"));
    assert_eq!(int(&event, "match_start"), 7);
    assert_eq!(int(&event, "match_end"), 12);
    assert_eq!(int(&event, "next_offset"), 12);
    finish(&controller, &handle_id);
}

#[tokio::test(flavor = "current_thread")]
async fn selects_stderr_and_matches_split_regex() {
    let (controller, _guard) = install_running();
    let started = start_background();
    let handle_id = string(&started, "handle_id");
    controller.append_stdout(b"ready on 1");
    controller.append_stderr(b"ready on");
    tokio::task::yield_now().await;
    controller.append_stderr(b" 4312");

    let mut request = wait_request(&handle_id, r"ready\s+on\s+\d+");
    request.insert("source".into(), vstr("stderr"));
    request.insert("regex".into(), VmValue::Bool(true));
    let event = require_dict(
        call_async("hostlib_tools_wait_command_output", request)
            .await
            .unwrap(),
    );
    assert_eq!(string(&event, "match"), "ready on 4");
    assert_eq!(int(&event, "match_start"), 0);
    finish(&controller, &handle_id);
}

#[tokio::test(flavor = "current_thread")]
async fn reports_exit_before_unmatched_output() {
    let (controller, _guard) = install_running();
    let started = start_background();
    let handle_id = string(&started, "handle_id");
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
    assert_eq!(string(&event, "status"), "exited");
    assert!(!boolean(&event, "matched"));
    let result = require_dict(event.get("result").cloned().expect("result"));
    assert_eq!(string(&result, "status"), "completed");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn timeout_uses_paused_time() {
    let (controller, _guard) = install_running();
    let started = start_background();
    let handle_id = string(&started, "handle_id");
    let mut request = wait_request(&handle_id, "ready");
    request.insert("timeout_ms".into(), VmValue::Int(1_000));
    let waiter =
        tokio::spawn(async move { call_async("hostlib_tools_wait_command_output", request).await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    let event = require_dict(waiter.await.unwrap().unwrap());
    assert_eq!(string(&event, "status"), "timed_out");
    assert!(boolean(&event, "timed_out"));
    finish(&controller, &handle_id);
}

#[tokio::test(flavor = "current_thread")]
async fn caller_cancellation_drops_wait_without_touching_process() {
    let (controller, _guard) = install_running();
    let started = start_background();
    let handle_id = string(&started, "handle_id");
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
    let handle_id = string(&started, "handle_id");
    let completion = register_completion_notifier(&handle_id).expect("live handle");
    let request = wait_request(&handle_id, "ready");
    let waiter =
        tokio::spawn(async move { call_async("hostlib_tools_wait_command_output", request).await });
    tokio::task::yield_now().await;
    controller.append_stdout(b"ready");
    controller.complete_with(ExitStatus::from_code(0));
    completion.recv().expect("background finalization");
    let event = require_dict(waiter.await.unwrap().unwrap());
    assert_eq!(string(&event, "status"), "matched");
}

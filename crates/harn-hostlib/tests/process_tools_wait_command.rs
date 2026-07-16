//! Focused mock coverage for `wait_command` direct result synchronization.

#![cfg(unix)]

use std::sync::Arc;

use harn_hostlib::process::{
    install_spawner, ExitStatus, MockHandleController, MockProcessConfig, MockSpawner, SpawnerGuard,
};
use harn_hostlib::tools::long_running::register_result_notifier;
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
    let entry = registry
        .find(builtin)
        .unwrap_or_else(|| panic!("builtin {builtin} not registered"));
    let arg = VmValue::dict(request);
    (entry.handler)(&[arg])
}

fn dict() -> harn_vm::value::DictMap {
    harn_vm::value::DictMap::new()
}

fn vstr(value: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(value))
}

fn vlist_str(values: &[&str]) -> VmValue {
    VmValue::List(Arc::new(values.iter().map(|s| vstr(s)).collect()))
}

fn require_dict(value: VmValue) -> harn_vm::value::DictMap {
    match value {
        VmValue::Dict(map) => (*map).clone(),
        other => panic!("expected dict response, got {other:?}"),
    }
}

fn require_str(map: &harn_vm::value::DictMap, key: &str) -> String {
    match map.get(key) {
        Some(VmValue::String(s)) => s.to_string(),
        other => panic!("expected string at {key}, got {other:?}"),
    }
}

fn install_mock_with(
    config: MockProcessConfig,
) -> (Arc<MockSpawner>, MockHandleController, SpawnerGuard) {
    let spawner = Arc::new(MockSpawner::new());
    let controller = spawner.enqueue(config);
    let guard = install_spawner(spawner.clone());
    (spawner, controller, guard)
}

fn unique_session_id(prefix: &str) -> String {
    format!(
        "{}-{}-{:?}",
        prefix,
        std::process::id(),
        std::thread::current().id()
    )
}

#[test]
fn wait_command_waits_for_live_handle_result_and_consumes_feedback() {
    let session_id = unique_session_id("test-wait-command-direct-result");
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);
    let (_spawner, controller, _guard) = install_mock_with(MockProcessConfig::running());

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sh", "-c", "echo direct"]));
    req.insert("background".into(), VmValue::Bool(true));
    let start = require_dict(call("hostlib_tools_run_command", req).unwrap());
    let handle_id = require_str(&start, "handle_id");

    let wait_handle_id = handle_id.clone();
    let wait_session_id = session_id.clone();
    let waiter = std::thread::spawn(move || {
        let _session_guard = harn_vm::agent_sessions::enter_current_session(wait_session_id);
        let mut wait_req = dict();
        wait_req.insert("handle_id".into(), vstr(&wait_handle_id));
        wait_req.insert("timeout_ms".into(), VmValue::Int(5_000));
        require_dict(call("hostlib_tools_wait_command", wait_req).unwrap())
    });

    controller.append_stdout(b"direct\n");
    controller.complete_with(ExitStatus::from_code(0));

    let waited = waiter.join().expect("waiter thread panicked");
    assert_eq!(require_str(&waited, "status"), "completed");
    assert_eq!(require_str(&waited, "feedback_kind"), "tool_result");
    assert_eq!(require_str(&waited, "handle_id"), handle_id);
    assert_eq!(require_str(&waited, "stdout"), "direct\n");

    let leftover = harn_vm::orchestration::agent_inbox::drain(&session_id);
    assert!(
        leftover.is_empty(),
        "explicit wait must consume matching tool_result feedback, got {leftover:?}"
    );
}

#[test]
fn long_running_result_notifier_fires_after_inbox_feedback_is_visible() {
    let session_id = unique_session_id("test-long-running-result-notifier-order");
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);
    let (_spawner, controller, _guard) = install_mock_with(MockProcessConfig::running());

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sh", "-c", "echo ordered"]));
    req.insert("background".into(), VmValue::Bool(true));
    let start = require_dict(call("hostlib_tools_run_command", req).unwrap());
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

#![cfg(unix)]

use std::sync::Arc;

use harn_hostlib::process::{install_spawner, MockProcessConfig, MockSpawner, SpawnerGuard};
use harn_hostlib::tools::long_running::register_completion_notifier;
use harn_hostlib::tools::ToolsCapability;
use harn_hostlib::{BuiltinRegistry, HostlibCapability, HostlibError};
use harn_vm::VmValue;

fn call(builtin: &str, request: harn_vm::value::DictMap) -> Result<VmValue, HostlibError> {
    harn_hostlib::tools::permissions::enable_for_test();
    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
    let entry = registry
        .find(builtin)
        .unwrap_or_else(|| panic!("builtin {builtin} not registered"));
    (entry.handler)(&[VmValue::dict(request)])
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

fn require_int(map: &harn_vm::value::DictMap, key: &str) -> i64 {
    match map.get(key) {
        Some(VmValue::Int(i)) => *i,
        other => panic!("expected int at {key}, got {other:?}"),
    }
}

fn require_str(map: &harn_vm::value::DictMap, key: &str) -> String {
    match map.get(key) {
        Some(VmValue::String(s)) => s.to_string(),
        other => panic!("expected string at {key}, got {other:?}"),
    }
}

fn require_bool(map: &harn_vm::value::DictMap, key: &str) -> bool {
    match map.get(key) {
        Some(VmValue::Bool(b)) => *b,
        other => panic!("expected bool at {key}, got {other:?}"),
    }
}

fn require_nested_dict(map: &harn_vm::value::DictMap, key: &str) -> harn_vm::value::DictMap {
    match map.get(key) {
        Some(VmValue::Dict(value)) => (**value).clone(),
        other => panic!("expected dict at {key}, got {other:?}"),
    }
}

fn snapshot_binding_fixture() -> harn_vm::value::DictMap {
    let mut files = dict();
    files.insert("src/lib.rs".into(), vstr("sha256:abc123"));

    let mut binding = dict();
    binding.insert("schema_version".into(), VmValue::Int(1));
    binding.insert("case_fingerprint".into(), vstr("case-123"));
    binding.insert("files".into(), VmValue::dict(files));
    binding
}

fn assert_snapshot_binding(map: &harn_vm::value::DictMap) {
    let binding = require_nested_dict(map, "snapshot_binding");
    assert_eq!(require_int(&binding, "schema_version"), 1);
    assert_eq!(require_str(&binding, "case_fingerprint"), "case-123");
    let files = require_nested_dict(&binding, "files");
    assert_eq!(require_str(&files, "src/lib.rs"), "sha256:abc123");
}

fn assert_response_matches_schema(method: &str, response: &VmValue) {
    let schema_body =
        harn_hostlib::schemas::lookup("tools", method, harn_hostlib::schemas::SchemaKind::Response)
            .unwrap_or_else(|| panic!("missing response schema for tools.{method}"));
    let schema_json: serde_json::Value =
        serde_json::from_str(schema_body).expect("response schema must be valid JSON");
    let schema = harn_vm::schema::json_to_vm_value(&schema_json);
    harn_vm::schema::validate_value_against_schema(response, &schema, false)
        .unwrap_or_else(|message| panic!("tools.{method} response schema mismatch: {message}"));
}

fn install_mock_with(config: MockProcessConfig) -> (Arc<MockSpawner>, SpawnerGuard) {
    let spawner = Arc::new(MockSpawner::new());
    let guard = install_spawner(spawner.clone());
    spawner.enqueue(config);
    (spawner, guard)
}

fn unique_session_id(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    )
}

fn cancel_handle(handle_id: &str) {
    let completion_rx = register_completion_notifier(handle_id);
    let mut cancel_req = dict();
    cancel_req.insert("handle_id".into(), vstr(handle_id));
    let cancel_resp = require_dict(call("hostlib_tools_cancel_handle", cancel_req).unwrap());
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
    let (_spawner, _guard) = install_mock_with(config);

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("background_after_ms".into(), VmValue::Int(50));
    req.insert("progress_max_inline_bytes".into(), VmValue::Int(200));
    req.insert(
        "snapshot_binding".into(),
        VmValue::dict(snapshot_binding_fixture()),
    );
    let resp_value = call("hostlib_tools_run_command", req).unwrap();
    assert_response_matches_schema("run_command", &resp_value);

    let resp = require_dict(resp_value);
    assert_eq!(require_str(&resp, "status"), "running");
    assert_eq!(require_str(&resp, "feedback_kind"), "tool_progress");
    assert_snapshot_binding(&resp);
    cancel_handle(&require_str(&resp, "handle_id"));
}

#[test]
fn run_command_background_after_progress_overlay_satisfies_response_schema() {
    let _session_guard = harn_vm::agent_sessions::enter_current_session(unique_session_id(
        "test-run-command-background-after-overlay-schema",
    ));
    let mut config = MockProcessConfig::running();
    config.stdout = b"tick\n".to_vec();
    let (_spawner, _guard) = install_mock_with(config);

    let wait_ms: i64 = 2000;
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("background_after_ms".into(), VmValue::Int(wait_ms));
    req.insert("progress_interval_ms".into(), VmValue::Int(1));
    req.insert("progress_max_inline_bytes".into(), VmValue::Int(200));
    req.insert(
        "snapshot_binding".into(),
        VmValue::dict(snapshot_binding_fixture()),
    );
    let resp_value = call("hostlib_tools_run_command", req).unwrap();
    assert_response_matches_schema("run_command", &resp_value);

    let resp = require_dict(resp_value);
    assert_eq!(require_str(&resp, "status"), "running");
    assert_eq!(require_str(&resp, "feedback_kind"), "tool_progress");
    assert_snapshot_binding(&resp);
    assert!(
        require_int(&resp, "duration_ms") < wait_ms,
        "expected progress-overlay duration below the wait budget"
    );
    cancel_handle(&require_str(&resp, "handle_id"));
}

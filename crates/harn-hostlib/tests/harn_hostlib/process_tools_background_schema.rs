#![cfg(unix)]

use std::sync::Arc;

use harn_hostlib::process::{
    install_spawner, ExitStatus, MockProcessConfig, MockSpawner, SpawnerGuard,
};
use harn_hostlib::tools::long_running::register_completion_notifier;
use harn_hostlib::tools::ToolsCapability;
use harn_hostlib::{BuiltinRegistry, HostlibCapability, HostlibError};
use harn_vm::VmValue;

fn call(builtin: &str, request: harn_vm::value::DictMap) -> Result<VmValue, HostlibError> {
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
    assert!(!require_str(&resp, "cwd").is_empty());
    assert_snapshot_binding(&resp);
    cancel_handle(&require_str(&resp, "handle_id"));
}

#[test]
fn run_command_background_after_progress_overlay_satisfies_response_schema() {
    let session_id = unique_session_id("test-run-command-background-after-overlay-schema");
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);
    let mut config = MockProcessConfig::running();
    config.stdout = b"tick\n".to_vec();
    let spawner = Arc::new(MockSpawner::new());
    let controller = spawner.enqueue(config);
    let _guard = install_spawner(spawner);

    harn_vm::orchestration::agent_inbox::push(&session_id, "notice", "first", "test");
    harn_vm::orchestration::agent_inbox::push(&session_id, "notice", "second", "test");
    let (driver_ready_tx, driver_ready_rx) = std::sync::mpsc::sync_channel::<()>(0);

    // Hold two progress entries locally while changing the output between
    // them, then restore them in their original order. This deterministically
    // proves the deadline response selects the latest progress without making
    // the production waiter observe or depend on test-side timing sleeps.
    let progress_session_id = session_id.clone();
    let progress_driver = std::thread::spawn(move || {
        let mut buffered = harn_vm::orchestration::agent_inbox::drain(&progress_session_id);
        assert_eq!(buffered.len(), 2);
        assert_eq!(buffered[0].content, "first");
        assert_eq!(buffered[0].sequence, 1);
        assert_eq!(buffered[1].content, "second");
        assert_eq!(buffered[1].sequence, 2);
        driver_ready_tx
            .send(())
            .expect("driver-ready receiver dropped");
        for _ in 0..2 {
            assert!(
                harn_vm::orchestration::agent_inbox::wait_sync(
                    &progress_session_id,
                    std::time::Duration::from_secs(5),
                ),
                "progress feedback was never published",
            );
            buffered.extend(harn_vm::orchestration::agent_inbox::drain(
                &progress_session_id,
            ));
            controller.append_stdout(b"latest\n");
        }
        buffered.sort_by_key(|entry| entry.sequence);
        for entry in buffered.into_iter().rev() {
            harn_vm::orchestration::agent_inbox::requeue_front(entry);
        }
    });
    driver_ready_rx
        .recv()
        .expect("progress driver did not start");

    let wait_ms: i64 = 500;
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("background_after_ms".into(), VmValue::Int(wait_ms));
    req.insert("progress_interval_ms".into(), VmValue::Int(1));
    req.insert("progress_max_inline_bytes".into(), VmValue::Int(200));
    req.insert(
        "snapshot_binding".into(),
        VmValue::dict(snapshot_binding_fixture()),
    );
    // This sync builtin is implemented with std::sync::mpsc::recv_timeout and
    // OS threads, so Tokio's paused clock cannot drive the boundary. Keep the
    // sole elapsed assertion deliberately broad and sleep-free. The 500ms
    // window also replaces the old nominal 2000ms fixture budget.
    let started = std::time::Instant::now();
    let resp_value = call("hostlib_tools_run_command", req).unwrap();
    let elapsed = started.elapsed();
    progress_driver.join().expect("progress driver panicked");
    assert_response_matches_schema("run_command", &resp_value);

    let resp = require_dict(resp_value);
    assert_eq!(require_str(&resp, "status"), "running");
    assert_eq!(require_str(&resp, "feedback_kind"), "tool_progress");
    assert!(!require_str(&resp, "cwd").is_empty());
    assert_snapshot_binding(&resp);
    assert!(
        require_str(&resp, "stdout").contains("latest\n"),
        "deadline response must use the latest buffered progress snapshot",
    );
    assert!(
        require_int(&resp, "duration_ms") < wait_ms,
        "expected progress-overlay duration below the wait budget"
    );
    assert!(
        elapsed >= std::time::Duration::from_millis(250),
        "progress must not end a {wait_ms}ms inline window early: {elapsed:?}",
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "inline deadline exceeded its broad scheduling allowance: {elapsed:?}",
    );
    let foreign = harn_vm::orchestration::agent_inbox::drain(&session_id);
    assert_eq!(foreign.len(), 2);
    assert_eq!(foreign[0].content, "first");
    assert_eq!(foreign[0].sequence, 1);
    assert_eq!(foreign[1].content, "second");
    assert_eq!(foreign[1].sequence, 2);
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

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["verify"]));
    req.insert("background_after_ms".into(), VmValue::Int(5_000));
    req.insert("progress_interval_ms".into(), VmValue::Int(1));
    req.insert(
        "snapshot_binding".into(),
        VmValue::dict(snapshot_binding_fixture()),
    );
    let resp_value = call("hostlib_tools_run_command", req).unwrap();
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

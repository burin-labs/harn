//! Integration tests for the process-lifecycle tool builtins
//! (`run_command`, `wait_command`, `run_test`, `run_build_command`,
//! `inspect_test_results`, `manage_packages`, `cancel_handle`).
//!
//! These tests are **deterministic and mock-based**. Every spawn goes
//! through a `MockSpawner` installed via
//! `harn_hostlib::process::install_spawner`, so:
//!
//! - No real subprocess is spawned.
//! - There is zero `std::thread::sleep` and zero `std::time::Instant::now()`
//!   polling — we use [`harn_hostlib::tools::long_running::register_completion_notifier`]
//!   to deterministically await the long-running waiter thread.
//! - Tests run in well under 50 ms each.
//!
//! End-to-end coverage of the real-process spawn path is provided by
//! `tests/harn_hostlib/process_tools_e2e.rs`. That suite is allowed to spawn real
//! subprocesses; if it grows, it should move into the slow E2E job
//! tracked by Tier 2A of the deflake epic (issue #1069).

#![cfg(unix)]

mod background;
mod build;
mod cwd;

use std::collections::BTreeMap;
use std::sync::Arc;

use harn_hostlib::process::{
    install_spawner, ExitStatus, MockHandleController, MockProcessConfig, MockSpawner,
    ProcessCleanupChild, ProcessCleanupReport, SpawnerGuard,
};
use harn_hostlib::tools::long_running::register_completion_notifier;
use harn_hostlib::tools::ToolsCapability;
use harn_hostlib::{BuiltinRegistry, HostlibCapability, HostlibError};
use harn_vm::orchestration::{
    pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
};
use harn_vm::VmValue;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

// -------- harness --------

fn registry() -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
    registry
}

fn call(builtin: &str, request: harn_vm::value::DictMap) -> Result<VmValue, HostlibError> {
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

fn require_nil(map: &harn_vm::value::DictMap, key: &str) {
    assert!(
        matches!(map.get(key), Some(VmValue::Nil)),
        "expected nil at {key}, got {:?}",
        map.get(key)
    );
}

fn require_nested_dict(map: &harn_vm::value::DictMap, key: &str) -> harn_vm::value::DictMap {
    match map.get(key) {
        Some(VmValue::Dict(value)) => (**value).clone(),
        other => panic!("expected dict at {key}, got {other:?}"),
    }
}

fn require_list(map: &harn_vm::value::DictMap, key: &str) -> Vec<VmValue> {
    match map.get(key) {
        Some(VmValue::List(value)) => value.as_ref().clone(),
        other => panic!("expected list at {key}, got {other:?}"),
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

fn as_dict(value: &VmValue) -> harn_vm::value::DictMap {
    match value {
        VmValue::Dict(map) => (**map).clone(),
        other => panic!("expected dict value, got {other:?}"),
    }
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

/// Install a fresh `MockSpawner` for the calling thread and return both the
/// spawner (for inspection / additional enqueues) and the `SpawnerGuard`
/// that restores the previous spawner on drop. The guard must be kept
/// alive for the duration of the test.
fn install_mock() -> (Arc<MockSpawner>, SpawnerGuard) {
    let spawner = Arc::new(MockSpawner::new());
    let guard = install_spawner(spawner.clone());
    (spawner, guard)
}

/// Convenience: install a mock and immediately enqueue a single config.
/// Returns the controller for the configured spawn plus the guard.
fn install_mock_with(
    config: MockProcessConfig,
) -> (Arc<MockSpawner>, MockHandleController, SpawnerGuard) {
    let (spawner, guard) = install_mock();
    let controller = spawner.enqueue(config);
    (spawner, controller, guard)
}

fn unique_session_id(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    )
}

fn cleanup_report_fixture(signal: i32) -> ProcessCleanupReport {
    ProcessCleanupReport {
        root_pid: Some(99_999),
        attempted_signals: vec![signal],
        children: vec![ProcessCleanupChild {
            pid: 100_001,
            parent_pid: Some(99_999),
            depth: 1,
            command_name: Some("sleep".to_string()),
            signals: vec![signal],
            alive_after_cleanup: Some(false),
        }],
    }
}

struct ExecutionPolicyGuard;

impl Drop for ExecutionPolicyGuard {
    fn drop(&mut self) {
        pop_execution_policy();
    }
}

fn install_confining_policy(root: &std::path::Path) -> ExecutionPolicyGuard {
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::OsHardened,
        workspace_roots: vec![root.to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });
    ExecutionPolicyGuard
}

// -------- run_command --------

#[test]
fn run_command_echoes_stdout_and_reports_exit_zero() {
    let (_spawner, _controller, _guard) =
        install_mock_with(MockProcessConfig::with_stdout(0, "hello\n"));

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["bash", "-c", "echo hello"]));
    let resp_value = call("hostlib_tools_run_command", req).unwrap();
    assert_response_matches_schema("run_command", &resp_value);
    let resp = require_dict(resp_value);

    assert_eq!(require_int(&resp, "exit_code"), 0);
    assert_eq!(require_str(&resp, "stdout").trim(), "hello");
    assert_eq!(require_str(&resp, "stderr"), "");
    assert!(!require_bool(&resp, "timed_out"));
    assert_eq!(require_str(&resp, "status"), "completed");
    assert!(require_str(&resp, "command_id").starts_with("cmd_"));
    assert!(require_int(&resp, "pid") > 0);
    assert!(require_int(&resp, "process_group_id") > 0);
    assert!(require_str(&resp, "started_at").contains('T'));
    assert!(require_str(&resp, "ended_at").contains('T'));
    assert!(require_str(&resp, "audit_id").starts_with("audit_cmd_"));
    assert!(matches!(resp.get("signal"), Some(VmValue::Nil)));
    assert!(require_int(&resp, "duration_ms") >= 0);

    let output_path = require_str(&resp, "output_path");
    assert_eq!(
        std::fs::read_to_string(&output_path).unwrap().trim(),
        "hello"
    );
    let digest = format!(
        "sha256:{}",
        hex::encode(Sha256::digest(std::fs::read(&output_path).unwrap()))
    );
    assert_eq!(require_str(&resp, "output_sha256"), digest);
    assert_eq!(require_int(&resp, "line_count"), 1);
    assert!(require_int(&resp, "byte_count") >= 6);
}

#[test]
fn run_command_propagates_nonzero_exit_code() {
    let (_spawner, _controller, _guard) = install_mock_with(MockProcessConfig::completed(7));

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["bash", "-c", "exit 7"]));
    let resp_value = call("hostlib_tools_run_command", req).unwrap();
    assert_response_matches_schema("run_command", &resp_value);
    let resp = require_dict(resp_value);
    assert_eq!(require_int(&resp, "exit_code"), 7);
    assert!(!require_bool(&resp, "timed_out"));
}

#[test]
fn run_command_projects_mid_run_sandbox_assessment_without_collapsing_coverage() {
    let workspace = tempdir().unwrap();
    let _policy = install_confining_policy(workspace.path());
    let (spawner, _guard) = install_mock();

    let mut denied = MockProcessConfig::completed(1);
    denied.stderr = b"write failed: Operation not permitted".to_vec();
    spawner.enqueue(denied);
    let mut ordinary = MockProcessConfig::completed(1);
    ordinary.stderr = b"compiler error: unknown name".to_vec();
    spawner.enqueue(ordinary);

    let mut denied_req = dict();
    denied_req.insert("argv".into(), vlist_str(&["fixture", "denied"]));
    denied_req.insert(
        "cwd".into(),
        vstr(workspace.path().to_string_lossy().as_ref()),
    );
    let denied_value = call("hostlib_tools_run_command", denied_req).unwrap();
    assert_response_matches_schema("run_command", &denied_value);
    let denied_result = require_dict(denied_value);
    let denied_sandbox = require_nested_dict(&denied_result, "sandbox");
    assert_eq!(
        require_str(&denied_sandbox, "denial_reporting"),
        "inferred_only"
    );
    let denial = require_nested_dict(&denied_result, "denial");
    assert_eq!(
        require_str(&denial, "schema"),
        "harn.process.sandbox_refusal.v1"
    );
    assert_eq!(require_str(&denial, "gate"), "process_sandbox");
    assert!(!require_str(&denial, "backend").is_empty());
    assert_eq!(require_str(&denial, "operation"), "unknown");
    require_nil(&denial, "resource");
    assert_eq!(require_list(&denial, "command").len(), 2);
    assert_eq!(
        require_str(&denial, "stderr_excerpt"),
        "write failed: Operation not permitted"
    );
    assert_eq!(require_int(&denial, "count"), 1);
    assert_eq!(require_str(&denial, "observability"), "inferred");
    assert!(!require_bool(&denial, "retryable"));

    let mut ordinary_req = dict();
    ordinary_req.insert("argv".into(), vlist_str(&["fixture", "ordinary"]));
    ordinary_req.insert(
        "cwd".into(),
        vstr(workspace.path().to_string_lossy().as_ref()),
    );
    let ordinary_value = call("hostlib_tools_run_command", ordinary_req).unwrap();
    assert_response_matches_schema("run_command", &ordinary_value);
    let ordinary_result = require_dict(ordinary_value);
    require_nil(&ordinary_result, "denial");
    let ordinary_sandbox = require_nested_dict(&ordinary_result, "sandbox");
    assert_eq!(
        require_str(&ordinary_sandbox, "denial_reporting"),
        "inferred_only"
    );
}

#[test]
fn run_command_pipes_stdin_into_child() {
    // Mock `cat`: emit nothing on stdout from the spawn-side, but capture
    // the bytes the spawn-side wrote to stdin and assert in two ways:
    //   1. The captured bytes hit the controller's stdin buffer.
    //   2. The SpawnSpec recorded `use_stdin = true`.
    let (spawner, controller, _guard) = install_mock_with(MockProcessConfig::completed(0));

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["cat"]));
    req.insert("stdin".into(), vstr("from-stdin"));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    assert_eq!(require_int(&resp, "exit_code"), 0);

    let captured = spawner.captured();
    assert_eq!(captured.len(), 1);
    assert!(captured[0].use_stdin, "stdin should be wired up in spec");

    assert_eq!(controller.stdin_written(), b"from-stdin");
}

#[test]
fn run_command_kills_child_when_timeout_elapses() {
    // No exit set + force_timeout = `wait_with_timeout` reports timeout
    // immediately, no wall-clock dependence.
    let config = MockProcessConfig {
        force_timeout: true,
        cleanup_report: Some(cleanup_report_fixture(9)),
        ..MockProcessConfig::running()
    };
    let (_spawner, _controller, _guard) = install_mock_with(config);

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "30"]));
    req.insert("timeout_ms".into(), VmValue::Int(150));
    let resp_value = call("hostlib_tools_run_command", req).unwrap();
    assert_response_matches_schema("run_command", &resp_value);
    let resp = require_dict(resp_value);
    assert!(require_bool(&resp, "timed_out"));
    assert_eq!(require_str(&resp, "status"), "timed_out");
    // Killed children report exit_code -1 + a signal name.
    assert!(matches!(resp.get("signal"), Some(VmValue::String(_))));
    let cleanup = require_nested_dict(&resp, "process_cleanup");
    assert_eq!(require_int(&cleanup, "root_pid"), 99_999);
    assert_eq!(require_int(&cleanup, "reaped_child_count"), 1);
    let reaped = require_list(&cleanup, "reaped_children");
    let child = as_dict(&reaped[0]);
    assert_eq!(require_int(&child, "pid"), 100_001);
    assert_eq!(require_str(&child, "command_name"), "sleep");
    assert!(child.get("command").is_none());
}

#[test]
fn run_command_times_out_when_descendant_keeps_stdout_pipe_open() {
    let config = MockProcessConfig {
        stdout: b"direct-child-output\n".to_vec(),
        exit_status: Some(ExitStatus::from_code(0)),
        cleanup_report: Some(cleanup_report_fixture(9)),
        stdout_hangs_after_exit_until_kill: true,
        ..MockProcessConfig::default()
    };
    let (_spawner, controller, _guard) = install_mock_with(config);

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["python3", "build-daemon.py"]));
    req.insert("timeout_ms".into(), VmValue::Int(1));
    let resp_value = call("hostlib_tools_run_command", req).unwrap();
    assert_response_matches_schema("run_command", &resp_value);
    let resp = require_dict(resp_value);

    assert!(controller.was_killed());
    assert!(require_bool(&resp, "timed_out"));
    assert_eq!(require_str(&resp, "status"), "timed_out");
    assert_eq!(require_int(&resp, "exit_code"), -1);
    assert_eq!(require_str(&resp, "signal"), "SIGKILL");
    assert_eq!(require_str(&resp, "stdout"), "direct-child-output\n");
    let cleanup = require_nested_dict(&resp, "process_cleanup");
    assert_eq!(require_int(&cleanup, "root_pid"), 99_999);
    assert_eq!(require_int(&cleanup, "reaped_child_count"), 1);
    let reaped = require_list(&cleanup, "reaped_children");
    let child = as_dict(&reaped[0]);
    assert_eq!(require_int(&child, "pid"), 100_001);
    assert_eq!(require_str(&child, "command_name"), "sleep");
    assert!(child.get("command").is_none());
}

#[test]
fn run_command_spawns_foreground_children_in_their_own_process_group() {
    // The interrupt path (scope cancel / deadline / VM drop) signals the
    // child's process group so grandchildren are reaped too — the spawn
    // must therefore request one.
    let (spawner, _controller, _guard) = install_mock_with(MockProcessConfig::completed(0));

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["true"]));
    require_dict(call("hostlib_tools_run_command", req).unwrap());

    let captured = spawner.captured();
    assert_eq!(captured.len(), 1);
    assert!(
        captured[0].configure_process_group,
        "foreground run_command must put its child in its own process group"
    );
}

#[test]
fn run_command_kills_child_when_scope_interrupt_fires() {
    // A pre-armed cancel token (the shape scope cancellation / deadline
    // expiry / VM drop takes by the time the builtin polls it) must kill a
    // still-running child and report `killed` — deterministically, with no
    // real subprocess.
    let (_spawner, controller, _guard) = install_mock_with(MockProcessConfig::running());

    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let _interrupt = harn_vm::op_interrupt::install(Some(cancel), None);

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "30"]));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    assert_eq!(require_str(&resp, "status"), "killed");
    assert!(!require_bool(&resp, "timed_out"));
    assert_eq!(require_int(&resp, "exit_code"), -1);
    let cleanup = require_nested_dict(&resp, "process_cleanup");
    assert_eq!(require_int(&cleanup, "root_pid"), 99_999);
    assert!(controller.was_killed(), "interrupt must kill the child");
}

#[test]
fn run_command_background_ignores_scope_interrupt() {
    // Background commands deliberately wait without polling the invoking
    // scope's interrupt state. Their lifetime is owned by the handle store;
    // explicit cancellation is the only path that should kill them.
    let (_spawner, controller, _guard) = install_mock_with(MockProcessConfig::running());

    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let _interrupt = harn_vm::op_interrupt::install(Some(cancel), None);

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "30"]));
    req.insert("background".into(), VmValue::Bool(true));
    let response = require_dict(call("hostlib_tools_run_command", req).unwrap());
    let handle_id = require_str(&response, "handle_id");
    assert_eq!(require_str(&response, "status"), "running");
    assert!(
        !controller.was_killed(),
        "scope interrupt must not kill background work"
    );

    let completion_rx = register_completion_notifier(&handle_id);
    let mut cancel_req = dict();
    cancel_req.insert("handle_id".into(), vstr(&handle_id));
    let cancel_response = require_dict(call("hostlib_tools_cancel_handle", cancel_req).unwrap());
    assert!(require_bool(&cancel_response, "cancelled"));
    assert!(
        controller.was_killed(),
        "explicit cancellation must kill background work"
    );
    completion_rx
        .expect("background handle should still be live")
        .recv()
        .expect("background waiter did not publish completion");
}

#[test]
fn run_command_surfaces_wait_errors() {
    let config = MockProcessConfig {
        wait_error: Some("wait blew up".to_string()),
        ..MockProcessConfig::completed(0)
    };
    let (_spawner, _controller, _guard) = install_mock_with(config);

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["true"]));
    let err = call("hostlib_tools_run_command", req).unwrap_err();
    assert!(
        matches!(err, HostlibError::Backend { message, .. } if message.contains("wait failed"))
    );
}

#[test]
fn run_command_capture_stderr_false_merges_into_stdout() {
    let config = MockProcessConfig {
        stdout: b"out\n".to_vec(),
        stderr: b"err\n".to_vec(),
        ..MockProcessConfig::default()
    };
    let (_spawner, _controller, _guard) = install_mock_with(config);

    let mut req = dict();
    req.insert(
        "argv".into(),
        vlist_str(&["bash", "-c", "echo out; echo err 1>&2"]),
    );
    req.insert("capture_stderr".into(), VmValue::Bool(false));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    let stdout = require_str(&resp, "stdout");
    assert!(stdout.contains("out"), "stdout was {stdout:?}");
    assert!(stdout.contains("err"), "stdout was {stdout:?}");
    assert_eq!(require_str(&resp, "stderr"), "");
}

#[test]
fn run_command_supports_explicit_shell_mode() {
    let (spawner, _controller, _guard) =
        install_mock_with(MockProcessConfig::with_stdout(0, "shell-ok\n"));

    let mut shell: harn_vm::value::DictMap = Default::default();
    shell.insert("id".into(), vstr("sh"));

    let mut req = dict();
    req.insert("mode".into(), vstr("shell"));
    req.insert("command".into(), vstr("echo shell-ok"));
    req.insert("shell".into(), VmValue::dict(shell));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    assert_eq!(require_str(&resp, "stdout").trim(), "shell-ok");

    // Shell mode resolves to a real shell argv on the spec — verify the
    // command flows through (echo shell-ok) somewhere in the args.
    let captured = spawner.captured();
    let argv_blob = format!("{} {}", captured[0].program, captured[0].args.join(" "));
    assert!(
        argv_blob.contains("echo shell-ok"),
        "unexpected resolved argv: {argv_blob:?}"
    );
}

#[test]
fn run_command_shell_mode_uses_default_shell_when_omitted() {
    let default_shell =
        harn_vm::shells::get_default_shell().expect("test host should expose a default shell");
    let (spawner, _controller, _guard) =
        install_mock_with(MockProcessConfig::with_stdout(0, "shell-default\n"));

    let mut req = dict();
    req.insert("mode".into(), vstr("shell"));
    req.insert("command".into(), vstr("echo shell-default"));

    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    assert_eq!(require_str(&resp, "stdout").trim(), "shell-default");

    let captured = spawner.captured();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].program, default_shell.path);
    assert_eq!(
        captured[0].args[default_shell.default_args.len()],
        "echo shell-default"
    );
}

#[test]
fn run_command_caps_inline_output_and_read_command_output_reads_artifact() {
    let payload = vec![b'x'; 2000];
    let (_spawner, _controller, _guard) =
        install_mock_with(MockProcessConfig::with_stdout(0, payload));

    let mut capture: harn_vm::value::DictMap = Default::default();
    capture.insert("max_inline_bytes".into(), VmValue::Int(8));

    let mut req = dict();
    req.insert(
        "argv".into(),
        vlist_str(&["bash", "-c", "for i in $(seq 1 2000); do printf x; done"]),
    );
    req.insert("capture".into(), VmValue::dict(capture));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    assert_eq!(require_str(&resp, "stdout").len(), 8);
    assert_eq!(require_int(&resp, "byte_count"), 2000);

    let mut read_req = dict();
    read_req.insert("command_id".into(), vstr(&require_str(&resp, "command_id")));
    read_req.insert("offset".into(), VmValue::Int(1990));
    read_req.insert("length".into(), VmValue::Int(20));
    let read_resp = require_dict(call("hostlib_tools_read_command_output", read_req).unwrap());
    assert_eq!(require_str(&read_resp, "content").len(), 10);
    assert!(require_bool(&read_resp, "eof"));
}

#[test]
fn read_command_output_rejects_arbitrary_path_reads() {
    let mut req = dict();
    req.insert("path".into(), vstr("/etc/passwd"));
    let err = call("hostlib_tools_read_command_output", req).unwrap_err();
    assert!(matches!(err, HostlibError::InvalidParameter { param, .. } if param == "path"));
}

#[test]
fn run_command_passes_env_when_supplied() {
    let (spawner, _controller, _guard) =
        install_mock_with(MockProcessConfig::with_stdout(0, "value-42\n"));

    let mut env_dict: harn_vm::value::DictMap = Default::default();
    env_dict.insert("PATH".into(), vstr("/bin:/usr/bin"));
    env_dict.insert("HOSTLIB_TEST_VAR".into(), vstr("value-42"));

    let mut req = dict();
    req.insert(
        "argv".into(),
        vlist_str(&["bash", "-c", "echo $HOSTLIB_TEST_VAR"]),
    );
    req.insert("env".into(), VmValue::dict(env_dict));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    assert_eq!(require_str(&resp, "stdout").trim(), "value-42");

    let captured = spawner.captured();
    assert_eq!(
        captured[0].env.get("HOSTLIB_TEST_VAR"),
        Some(&"value-42".to_string())
    );
}

#[test]
fn run_command_missing_argv_returns_missing_parameter() {
    // No mock needed — fails before reaching the spawner.
    let err = call("hostlib_tools_run_command", dict()).unwrap_err();
    match err {
        HostlibError::MissingParameter { param, .. } => assert_eq!(param, "argv"),
        other => panic!("expected MissingParameter, got {other:?}"),
    }
}

#[test]
fn run_command_empty_argv_returns_invalid_parameter() {
    let mut req = dict();
    req.insert("argv".into(), VmValue::List(Arc::new(Vec::new())));
    let err = call("hostlib_tools_run_command", req).unwrap_err();
    assert!(matches!(err, HostlibError::InvalidParameter { param, .. } if param == "argv"));
}

#[test]
fn run_command_rejects_nonexistent_cwd() {
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["true"]));
    req.insert("cwd".into(), vstr("/this/does/not/exist/anywhere"));
    let err = call("hostlib_tools_run_command", req).unwrap_err();
    assert!(matches!(err, HostlibError::InvalidParameter { param, .. } if param == "cwd"));
}

#[test]
fn run_command_rejects_malformed_cwd() {
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["true"]));
    req.insert("cwd".into(), VmValue::Bool(true));
    let err = call("hostlib_tools_run_command", req).unwrap_err();
    assert!(matches!(err, HostlibError::InvalidParameter { param, .. } if param == "cwd"));
}

#[test]
fn run_command_argv_must_be_strings() {
    let mut req = dict();
    req.insert(
        "argv".into(),
        VmValue::List(Arc::new(vec![VmValue::Int(1)])),
    );
    let err = call("hostlib_tools_run_command", req).unwrap_err();
    assert!(matches!(err, HostlibError::InvalidParameter { param, .. } if param == "argv"));
}

#[test]
fn run_command_rejects_out_of_range_capture_limit() {
    let mut capture: BTreeMap<String, VmValue> = BTreeMap::new();
    capture.insert("max_inline_bytes".into(), VmValue::Float(1.0e100));

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["true"]));
    req.insert("capture".into(), VmValue::dict(capture));
    let err = call("hostlib_tools_run_command", req).unwrap_err();
    assert!(
        matches!(err, HostlibError::InvalidParameter { param, .. } if param == "max_inline_bytes")
    );
}

// -------- run_test --------

#[test]
fn run_test_explicit_argv_runs_and_returns_handle() {
    let (_spawner, _controller, _guard) = install_mock_with(MockProcessConfig::completed(0));

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["true"]));
    let resp = require_dict(call("hostlib_tools_run_test", req).unwrap());
    assert_eq!(require_int(&resp, "exit_code"), 0);
    assert!(!require_str(&resp, "result_handle").is_empty());
}

#[test]
fn run_test_without_argv_or_manifest_errors() {
    let dir = tempdir().unwrap();
    let mut req = dict();
    req.insert("cwd".into(), vstr(dir.path().to_str().unwrap()));
    let err = call("hostlib_tools_run_test", req).unwrap_err();
    assert!(matches!(err, HostlibError::InvalidParameter { param, .. } if param == "argv"));
}

#[test]
fn run_test_inspect_returns_parsed_records_for_explicit_junit() {
    // Mock a cargo libtest-style stdout that the parser auto-detects.
    let stdout = "running 2 tests\n\
                  test a::passes ... ok\n\
                  test a::fails ... FAILED\n\
                  \n\
                  test result: FAILED. 1 passed; 1 failed; 0 ignored\n";
    let (_spawner, _controller, _guard) =
        install_mock_with(MockProcessConfig::with_stdout(1, stdout));

    let mut req = dict();
    req.insert(
        "argv".into(),
        vlist_str(&["bash", "-c", "echo cargo libtest output"]),
    );
    let resp = require_dict(call("hostlib_tools_run_test", req).unwrap());
    assert_eq!(require_int(&resp, "exit_code"), 1);
    let handle = require_str(&resp, "result_handle");

    let mut inspect_req = dict();
    inspect_req.insert("result_handle".into(), vstr(&handle));
    inspect_req.insert("include_passing".into(), VmValue::Bool(true));
    let inspect = require_dict(call("hostlib_tools_inspect_test_results", inspect_req).unwrap());
    assert_eq!(require_str(&inspect, "result_handle"), handle);
    let tests = match inspect.get("tests") {
        Some(VmValue::List(l)) => (**l).clone(),
        other => panic!("expected list, got {other:?}"),
    };
    assert_eq!(tests.len(), 2);
}

#[test]
fn run_test_summary_omitted_when_no_records_parsed() {
    let (_spawner, _controller, _guard) =
        install_mock_with(MockProcessConfig::with_stdout(0, "nothing\n"));

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["bash", "-c", "echo nothing"]));
    let resp = require_dict(call("hostlib_tools_run_test", req).unwrap());
    assert!(!resp.contains_key("summary"));
}

// -------- inspect_test_results --------

#[test]
fn inspect_test_results_unknown_handle_errors() {
    let mut req = dict();
    req.insert(
        "result_handle".into(),
        vstr("htr-deadbeef-this-is-not-real"),
    );
    let err = call("hostlib_tools_inspect_test_results", req).unwrap_err();
    assert!(
        matches!(err, HostlibError::InvalidParameter { param, .. } if param == "result_handle")
    );
}

#[test]
fn inspect_test_results_missing_handle_errors() {
    let err = call("hostlib_tools_inspect_test_results", dict()).unwrap_err();
    assert!(
        matches!(err, HostlibError::MissingParameter { param, .. } if param == "result_handle")
    );
}

// -------- manage_packages --------

#[test]
fn manage_packages_missing_operation_errors() {
    let err = call("hostlib_tools_manage_packages", dict()).unwrap_err();
    assert!(matches!(err, HostlibError::MissingParameter { param, .. } if param == "operation"));
}

#[test]
fn manage_packages_unknown_operation_errors() {
    let mut req = dict();
    req.insert("operation".into(), vstr("frobnicate"));
    req.insert("ecosystem".into(), vstr("npm"));
    let err = call("hostlib_tools_manage_packages", req).unwrap_err();
    assert!(matches!(err, HostlibError::InvalidParameter { param, .. } if param == "operation"));
}

#[test]
fn manage_packages_no_ecosystem_no_manifest_errors() {
    let dir = tempdir().unwrap();
    let mut req = dict();
    req.insert("operation".into(), vstr("install"));
    req.insert("cwd".into(), vstr(dir.path().to_str().unwrap()));
    let err = call("hostlib_tools_manage_packages", req).unwrap_err();
    assert!(matches!(err, HostlibError::InvalidParameter { param, .. } if param == "ecosystem"));
}

#[test]
fn manage_packages_unsupported_pair_for_ecosystem_errors() {
    let mut req = dict();
    req.insert("operation".into(), vstr("add"));
    req.insert("ecosystem".into(), vstr("gradle"));
    req.insert("packages".into(), vlist_str(&["junit"]));
    let err = call("hostlib_tools_manage_packages", req).unwrap_err();
    assert!(matches!(err, HostlibError::InvalidParameter { param, .. } if param == "operation"));
}

#[test]
fn manage_packages_runs_for_detected_ecosystem_with_explicit_cwd() {
    let (spawner, _controller, _guard) = install_mock_with(MockProcessConfig::completed(0));

    let dir = tempdir().unwrap();
    let mut req = dict();
    req.insert("operation".into(), vstr("update"));
    req.insert("ecosystem".into(), vstr("bundler"));
    req.insert("cwd".into(), vstr(dir.path().to_str().unwrap()));
    let resp = require_dict(call("hostlib_tools_manage_packages", req).unwrap());
    assert_eq!(require_str(&resp, "ecosystem"), "bundler");
    assert_eq!(require_str(&resp, "operation"), "update");
    assert!(matches!(
        resp.get("lockfile_changed"),
        Some(VmValue::Bool(_))
    ));
    // Spec captured `bundle update`.
    let captured = spawner.captured();
    assert_eq!(captured[0].program, "bundle");
    assert_eq!(captured[0].args, vec!["update".to_string()]);
}

// -------- universal catastrophic-command floor --------
//
// The floor is enforced UNCONDITIONALLY at `spawn_process` (no command_policy
// pushed), so every hostlib process tool inherits it. Catastrophic commands are
// blocked before spawn; benign commands proceed. See
// `harn_vm::orchestration::universal_catastrophic_reason`.

fn run_command_argv(argv: &[&str]) -> (Arc<MockSpawner>, Result<VmValue, HostlibError>) {
    let (spawner, _guard) = install_mock();
    // Enqueue a benign completion so that IF the floor were bypassed the call
    // would succeed via the mock — making an assertion failure unambiguous.
    spawner.enqueue(MockProcessConfig::with_stdout(0, "ok\n"));
    let mut req = dict();
    req.insert("argv".into(), vlist_str(argv));
    let result = call("hostlib_tools_run_command", req);
    // Guard drops at end of scope via the tuple; keep spawner for inspection.
    (spawner, result)
}

#[test]
fn run_command_blocks_universal_catastrophes_before_spawn() {
    for argv in [
        vec!["rm", "-rf", "/"],
        vec!["sh", "-c", ":(){ :|:& };:"],
        vec!["mkfs.ext4", "/dev/sda"],
        vec!["dd", "of=/dev/sda", "if=/dev/zero"],
    ] {
        let (spawner, result) = run_command_argv(&argv);
        match result {
            Err(HostlibError::CatastrophicFloor { message, .. }) => {
                assert!(!message.is_empty(), "floor reason should be non-empty");
            }
            other => panic!("expected CatastrophicFloor for {argv:?}, got {other:?}"),
        }
        assert!(
            spawner.captured().is_empty(),
            "catastrophic command {argv:?} must NEVER be spawned"
        );
    }
}

#[test]
fn run_command_blocks_git_destructive_family_before_spawn() {
    for argv in [
        vec!["git", "reset", "--hard"],
        vec!["git", "clean", "-fdx"],
        vec![
            "git",
            "push",
            "--force-with-lease=main:abc123",
            "origin",
            "HEAD",
        ],
        vec!["sh", "-c", "git reset --hard"],
    ] {
        let (spawner, result) = run_command_argv(&argv);
        match result {
            Err(HostlibError::CatastrophicFloor { message, .. }) => {
                assert!(!message.is_empty(), "floor reason should be non-empty");
            }
            other => panic!("expected CatastrophicFloor for {argv:?}, got {other:?}"),
        }
        assert_eq!(
            spawner.captured().len(),
            0,
            "git-destructive command {argv:?} must NEVER be spawned"
        );
    }
}

#[test]
fn run_command_allows_benign_command() {
    let (spawner, result) = run_command_argv(&["ls", "-la"]);
    let resp = require_dict(result.unwrap());
    assert_eq!(require_int(&resp, "exit_code"), 0);
    assert_eq!(
        spawner.captured().len(),
        1,
        "benign command must be spawned"
    );
}

#[test]
fn run_command_allows_scoped_delete_followed_by_cmake_configure() {
    let (spawner, result) = run_command_argv(&[
        "sh",
        "-c",
        "rm -rf build/burin-eval-setup && if command -v ninja >/dev/null 2>&1; then cmake -S . -B build/burin-eval-setup -G Ninja; else cmake -S . -B build/burin-eval-setup; fi",
    ]);
    let resp = require_dict(result.unwrap());
    assert_eq!(require_int(&resp, "exit_code"), 0);
    assert_eq!(
        spawner.captured().len(),
        1,
        "scoped build-dir cleanup must be spawned"
    );
}

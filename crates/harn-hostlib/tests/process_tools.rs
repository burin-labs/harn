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
//! `tests/process_tools_e2e.rs`. That suite is allowed to spawn real
//! subprocesses; if it grows, it should move into the slow E2E job
//! tracked by Tier 2A of the deflake epic (issue #1069).

#![cfg(unix)]

use std::collections::BTreeMap;
use std::sync::Arc;

use harn_hostlib::process::{
    install_spawner, ExitStatus, MockHandleController, MockProcessConfig, MockSpawner,
    ProcessCleanupChild, ProcessCleanupReport, ProcessError, SpawnerGuard,
};
use harn_hostlib::tools::long_running::register_completion_notifier;
use harn_hostlib::tools::ToolsCapability;
use harn_hostlib::{BuiltinRegistry, HostlibCapability, HostlibError};
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

// -------- toolchain_facts --------

#[test]
fn toolchain_facts_runs_config_declared_probes_and_cache_state() {
    let (spawner, guard) = install_mock();
    let _cargo = spawner.enqueue(MockProcessConfig::with_stdout(
        0,
        "cargo 1.90.0 (1159e78c4 2025-09-14)\n",
    ));
    let _zig = spawner.enqueue(MockProcessConfig::with_stdout(0, "0.13.0\n"));

    let workspace = tempdir().unwrap();
    let cargo_home = workspace.path().join(".cargo-home");
    let zig_cache = workspace.path().join(".zig-cache");
    std::fs::create_dir_all(&cargo_home).unwrap();
    std::fs::create_dir_all(&zig_cache).unwrap();

    let mut cargo_version = dict();
    cargo_version.insert("parser".into(), vstr("regex"));
    cargo_version.insert("pattern".into(), vstr(r"cargo\s+([0-9.]+)"));
    cargo_version.insert("group".into(), VmValue::Int(1));

    let mut cargo_env = dict();
    cargo_env.insert(
        "CARGO_HOME".into(),
        vstr(cargo_home.to_string_lossy().as_ref()),
    );

    let mut cargo_probe = dict();
    cargo_probe.insert("name".into(), vstr("cargo"));
    cargo_probe.insert("argv".into(), vlist_str(&["cargo", "--version"]));
    cargo_probe.insert("version".into(), VmValue::dict(cargo_version));
    cargo_probe.insert("env".into(), VmValue::dict(cargo_env));
    cargo_probe.insert("env_mode".into(), vstr("replace"));
    cargo_probe.insert("cache_env".into(), vlist_str(&["CARGO_HOME"]));

    let mut zig_probe = dict();
    zig_probe.insert("name".into(), vstr("zig-fixture"));
    zig_probe.insert("argv".into(), vlist_str(&["zig", "version"]));
    zig_probe.insert(
        "cwd".into(),
        vstr(workspace.path().to_string_lossy().as_ref()),
    );
    zig_probe.insert(
        "env".into(),
        VmValue::dict({
            let mut env = dict();
            env.insert(
                "ZIG_LOCAL_CACHE_DIR".into(),
                vstr(zig_cache.to_string_lossy().as_ref()),
            );
            env
        }),
    );
    zig_probe.insert("env_mode".into(), vstr("replace"));
    zig_probe.insert(
        "cache_env".into(),
        vlist_str(&["ZIG_LOCAL_CACHE_DIR", "UNSET_CACHE"]),
    );
    zig_probe.insert("state_paths".into(), vlist_str(&[".zig-cache"]));

    let mut req = dict();
    req.insert(
        "probes".into(),
        VmValue::List(Arc::new(vec![
            VmValue::dict(cargo_probe),
            VmValue::dict(zig_probe),
        ])),
    );
    let resp = require_dict(call("hostlib_tools_toolchain_facts", req).unwrap());
    let toolchains = require_list(&resp, "toolchains");
    assert_eq!(toolchains.len(), 2);

    let cargo = as_dict(&toolchains[0]);
    assert_eq!(require_str(&cargo, "name"), "cargo");
    assert_eq!(require_str(&cargo, "status"), "ok");
    assert!(require_bool(&cargo, "available"));
    assert_eq!(require_str(&cargo, "version"), "1.90.0");
    assert_eq!(require_int(&cargo, "exit_code"), 0);
    let cargo_cache = require_nested_dict(&cargo, "cache_env");
    let cargo_home_fact = require_nested_dict(&cargo_cache, "CARGO_HOME");
    assert_eq!(require_str(&cargo_home_fact, "source"), "probe_env");
    assert_eq!(
        require_str(&cargo_home_fact, "value"),
        cargo_home.to_string_lossy().as_ref()
    );

    let zig = as_dict(&toolchains[1]);
    assert_eq!(require_str(&zig, "name"), "zig-fixture");
    assert_eq!(require_str(&zig, "status"), "ok");
    assert_eq!(require_str(&zig, "version"), "0.13.0");
    let zig_cache_facts = require_nested_dict(&zig, "cache_env");
    let zig_cache_fact = require_nested_dict(&zig_cache_facts, "ZIG_LOCAL_CACHE_DIR");
    assert_eq!(require_str(&zig_cache_fact, "source"), "probe_env");
    assert_eq!(
        require_str(&zig_cache_fact, "value"),
        zig_cache.to_string_lossy().as_ref()
    );
    let unset_cache = require_nested_dict(&zig_cache_facts, "UNSET_CACHE");
    assert_eq!(require_str(&unset_cache, "source"), "unset");
    require_nil(&unset_cache, "value");

    let state_paths = require_list(&zig, "state_paths");
    let zig_state = as_dict(&state_paths[0]);
    assert_eq!(require_str(&zig_state, "path"), ".zig-cache");
    assert!(require_bool(&zig_state, "exists"));
    assert_eq!(require_str(&zig_state, "kind"), "directory");

    let captured = spawner.captured();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0].program, "cargo");
    assert_eq!(captured[0].args, vec!["--version".to_string()]);
    assert_eq!(captured[1].program, "zig");
    assert_eq!(captured[1].args, vec!["version".to_string()]);
    drop(guard);
}

#[test]
fn toolchain_facts_reports_spawn_failure_without_aborting_batch() {
    let (spawner, guard) = install_mock();
    spawner.enqueue(MockProcessConfig {
        spawn_error: Some(ProcessError::Spawn("program not found".to_string())),
        ..MockProcessConfig::default()
    });

    let mut probe = dict();
    probe.insert("name".into(), vstr("missing-tool"));
    probe.insert("argv".into(), vlist_str(&["missing-tool", "--version"]));

    let mut req = dict();
    req.insert(
        "probes".into(),
        VmValue::List(Arc::new(vec![VmValue::dict(probe)])),
    );
    let resp = require_dict(call("hostlib_tools_toolchain_facts", req).unwrap());
    let toolchains = require_list(&resp, "toolchains");
    let missing = as_dict(&toolchains[0]);
    assert_eq!(require_str(&missing, "name"), "missing-tool");
    assert_eq!(require_str(&missing, "status"), "spawn_failed");
    assert!(!require_bool(&missing, "available"));
    require_nil(&missing, "version");
    require_nil(&missing, "exit_code");
    assert!(require_str(&missing, "error").contains("program not found"));
    drop(guard);
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
fn run_command_runs_in_supplied_cwd() {
    let (spawner, _controller, _guard) = install_mock_with(MockProcessConfig::completed(0));

    let dir = tempdir().unwrap();
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["bash", "-c", "pwd"]));
    req.insert("cwd".into(), vstr(dir.path().to_str().unwrap()));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    assert_eq!(require_int(&resp, "exit_code"), 0);
    let captured = spawner.captured();
    assert_eq!(captured.len(), 1);
    let canon_cwd = std::fs::canonicalize(dir.path()).unwrap();
    assert_eq!(captured[0].cwd.as_ref().unwrap(), &canon_cwd);
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

// -------- run_build_command --------

#[test]
fn run_build_command_explicit_argv_runs_and_parses_diagnostics() {
    let config = MockProcessConfig {
        stderr: b"src/foo.rs:3:7: error: parse error here\n".to_vec(),
        ..MockProcessConfig::completed(2)
    };
    let (_spawner, _controller, _guard) = install_mock_with(config);

    let mut req = dict();
    req.insert(
        "argv".into(),
        vlist_str(&[
            "bash",
            "-c",
            "echo 'src/foo.rs:3:7: error: parse error here' 1>&2; exit 2",
        ]),
    );
    let resp = require_dict(call("hostlib_tools_run_build_command", req).unwrap());
    assert_eq!(require_int(&resp, "exit_code"), 2);
    let diagnostics = match resp.get("diagnostics") {
        Some(VmValue::List(l)) => (**l).clone(),
        other => panic!("expected list, got {other:?}"),
    };
    assert!(!diagnostics.is_empty());
}

#[test]
fn run_build_command_without_argv_or_manifest_errors() {
    let dir = tempdir().unwrap();
    let mut req = dict();
    req.insert("cwd".into(), vstr(dir.path().to_str().unwrap()));
    let err = call("hostlib_tools_run_build_command", req).unwrap_err();
    assert!(matches!(err, HostlibError::InvalidParameter { param, .. } if param == "argv"));
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

// -------- long_running handles --------

#[test]
fn run_command_long_running_returns_handle_immediately() {
    let _session_guard = harn_vm::agent_sessions::enter_current_session(unique_session_id(
        "test-run-command-long-running",
    ));
    // Stay running until the test cancels.
    let (_spawner, _controller, _guard) = install_mock_with(MockProcessConfig::running());

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("long_running".into(), VmValue::Bool(true));
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
fn run_command_long_running_reports_nil_process_group_when_unavailable() {
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
fn run_command_background_after_snapshot_satisfies_response_schema() {
    // Falsifier for the closed run_command response schema on the
    // background-after SNAPSHOT path (no initial progress feedback arrives).
    // Before the single-owner fix this response carried `feedback_kind`, a key
    // the closed schema did not declare, so validating it against the schema
    // the registry enforces for background dispatches was rejected.
    let _session_guard = harn_vm::agent_sessions::enter_current_session(unique_session_id(
        "test-run-command-background-after-snapshot-schema",
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
    let resp_value = call("hostlib_tools_run_command", req).unwrap();
    assert_response_matches_schema("run_command", &resp_value);

    let resp = require_dict(resp_value);
    assert_eq!(require_str(&resp, "status"), "running");
    assert_eq!(require_str(&resp, "feedback_kind"), "tool_progress");
    assert_snapshot_binding(&resp);

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
fn run_command_background_after_progress_overlay_satisfies_response_schema() {
    // Falsifier for the background-after OVERLAY path: an initial progress
    // feedback entry lands within the wait window. Before the single-owner fix
    // this path returned the raw agent-inbox progress payload, which omits
    // schema-required keys (pid, timed_out, output_sha256, sandbox, audit_id),
    // so schema validation rejected it. The fix overlays the progress payload
    // onto the canonical snapshot response instead.
    let _session_guard = harn_vm::agent_sessions::enter_current_session(unique_session_id(
        "test-run-command-background-after-overlay-schema",
    ));
    let mut config = MockProcessConfig::running();
    config.stdout = b"tick\n".to_vec();
    let (_spawner, _controller, _guard) = install_mock_with(config);

    // A tiny progress interval guarantees a matching progress entry lands well
    // before this generous wait budget elapses, so the overlay branch runs.
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
    // The overlay carries the progress payload's real elapsed duration, which
    // is strictly below the full wait budget the snapshot-only path reports.
    // This confirms the progress payload — not the bare snapshot — produced
    // this response.
    assert!(
        require_int(&resp, "duration_ms") < wait_ms,
        "expected progress-overlay duration below the wait budget"
    );

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
fn run_test_long_running_returns_handle() {
    let _session_guard = harn_vm::agent_sessions::enter_current_session(unique_session_id(
        "test-run-test-long-running",
    ));
    let (_spawner, _controller, _guard) = install_mock_with(MockProcessConfig::running());

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("long_running".into(), VmValue::Bool(true));
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
fn run_build_command_long_running_returns_handle() {
    let _session_guard = harn_vm::agent_sessions::enter_current_session(unique_session_id(
        "test-run-build-long-running",
    ));
    let (_spawner, _controller, _guard) = install_mock_with(MockProcessConfig::running());

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("long_running".into(), VmValue::Bool(true));
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

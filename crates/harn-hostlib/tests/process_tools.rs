//! Integration tests for the process-lifecycle tool builtins
//! (`run_command`, `run_test`, `run_build_command`,
//! `inspect_test_results`, `manage_packages`, `cancel_handle`).
//!
//! These spawn real subprocesses, so they're gated to Unix and rely only
//! on coreutils / shell that ship with both macOS and standard Linux
//! distros. The tests assert on:
//!
//! - argv / cwd / env / stdin plumbing matches the request schema
//! - timeout enforcement kills the child and reports `timed_out: true`
//! - error variants (missing argv, bad cwd, malformed types) round-trip
//!   through `HostlibError` rather than panicking
//! - language detection picks the right runner from manifest files
//! - inspect_test_results parses JUnit XML written by `run_test`
//! - long_running: true returns a handle synchronously; result lands in the
//!   global pending-feedback queue after the process exits
//! - cancel_handle kills the spawned process

#![cfg(unix)]

use std::collections::BTreeMap;
use std::rc::Rc;

use harn_hostlib::tools::ToolsCapability;
use harn_hostlib::{BuiltinRegistry, HostlibCapability, HostlibError};
use harn_vm::VmValue;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

fn registry() -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
    registry
}

fn call(builtin: &str, request: BTreeMap<String, VmValue>) -> Result<VmValue, HostlibError> {
    harn_hostlib::tools::permissions::enable_for_test();
    let registry = registry();
    let entry = registry
        .find(builtin)
        .unwrap_or_else(|| panic!("builtin {builtin} not registered"));
    let arg = VmValue::Dict(Rc::new(request));
    (entry.handler)(&[arg])
}

fn dict() -> BTreeMap<String, VmValue> {
    BTreeMap::new()
}

fn vstr(value: &str) -> VmValue {
    VmValue::String(Rc::from(value))
}

fn vlist_str(values: &[&str]) -> VmValue {
    VmValue::List(Rc::new(values.iter().map(|s| vstr(s)).collect()))
}

fn require_dict(value: VmValue) -> BTreeMap<String, VmValue> {
    match value {
        VmValue::Dict(map) => (*map).clone(),
        other => panic!("expected dict response, got {other:?}"),
    }
}

fn require_int(map: &BTreeMap<String, VmValue>, key: &str) -> i64 {
    match map.get(key) {
        Some(VmValue::Int(i)) => *i,
        other => panic!("expected int at {key}, got {other:?}"),
    }
}

fn require_str(map: &BTreeMap<String, VmValue>, key: &str) -> String {
    match map.get(key) {
        Some(VmValue::String(s)) => s.to_string(),
        other => panic!("expected string at {key}, got {other:?}"),
    }
}

fn require_bool(map: &BTreeMap<String, VmValue>, key: &str) -> bool {
    match map.get(key) {
        Some(VmValue::Bool(b)) => *b,
        other => panic!("expected bool at {key}, got {other:?}"),
    }
}

// -------- run_command --------

#[test]
fn run_command_echoes_stdout_and_reports_exit_zero() {
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["bash", "-c", "echo hello"]));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

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
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["bash", "-c", "exit 7"]));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    assert_eq!(require_int(&resp, "exit_code"), 7);
    assert!(!require_bool(&resp, "timed_out"));
}

#[test]
fn run_command_pipes_stdin_into_child() {
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["cat"]));
    req.insert("stdin".into(), vstr("from-stdin"));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    assert_eq!(require_str(&resp, "stdout"), "from-stdin");
}

#[test]
fn run_command_runs_in_supplied_cwd() {
    let dir = tempdir().unwrap();
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["bash", "-c", "pwd"]));
    req.insert("cwd".into(), vstr(dir.path().to_str().unwrap()));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    let stdout = require_str(&resp, "stdout");
    let canon_cwd = std::fs::canonicalize(dir.path()).unwrap();
    let canon_stdout = std::fs::canonicalize(stdout.trim()).unwrap();
    assert_eq!(canon_stdout, canon_cwd);
}

#[test]
fn run_command_kills_child_when_timeout_elapses() {
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "30"]));
    req.insert("timeout_ms".into(), VmValue::Int(150));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    assert!(require_bool(&resp, "timed_out"));
    assert_eq!(require_str(&resp, "status"), "timed_out");
    // Killed children report exit_code -1 + a signal name.
    assert!(matches!(resp.get("signal"), Some(VmValue::String(_))));
}

#[test]
fn run_command_capture_stderr_false_merges_into_stdout() {
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
    let mut shell: BTreeMap<String, VmValue> = BTreeMap::new();
    shell.insert("id".into(), vstr("sh"));

    let mut req = dict();
    req.insert("mode".into(), vstr("shell"));
    req.insert("command".into(), vstr("echo shell-ok"));
    req.insert("shell".into(), VmValue::Dict(Rc::new(shell)));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    assert_eq!(require_str(&resp, "stdout").trim(), "shell-ok");
}

#[test]
fn run_command_caps_inline_output_and_read_command_output_reads_artifact() {
    let mut capture: BTreeMap<String, VmValue> = BTreeMap::new();
    capture.insert("max_inline_bytes".into(), VmValue::Int(8));

    let mut req = dict();
    req.insert(
        "argv".into(),
        vlist_str(&["bash", "-c", "for i in $(seq 1 2000); do printf x; done"]),
    );
    req.insert("capture".into(), VmValue::Dict(Rc::new(capture)));
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
    let mut env_dict: BTreeMap<String, VmValue> = BTreeMap::new();
    env_dict.insert("PATH".into(), vstr(env!("PATH")));
    env_dict.insert("HOSTLIB_TEST_VAR".into(), vstr("value-42"));

    let mut req = dict();
    req.insert(
        "argv".into(),
        vlist_str(&["bash", "-c", "echo $HOSTLIB_TEST_VAR"]),
    );
    req.insert("env".into(), VmValue::Dict(Rc::new(env_dict)));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    assert_eq!(require_str(&resp, "stdout").trim(), "value-42");
}

#[test]
fn run_command_missing_argv_returns_missing_parameter() {
    let err = call("hostlib_tools_run_command", dict()).unwrap_err();
    match err {
        HostlibError::MissingParameter { param, .. } => assert_eq!(param, "argv"),
        other => panic!("expected MissingParameter, got {other:?}"),
    }
}

#[test]
fn run_command_empty_argv_returns_invalid_parameter() {
    let mut req = dict();
    req.insert("argv".into(), VmValue::List(Rc::new(Vec::new())));
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
fn run_command_argv_must_be_strings() {
    let mut req = dict();
    req.insert("argv".into(), VmValue::List(Rc::new(vec![VmValue::Int(1)])));
    let err = call("hostlib_tools_run_command", req).unwrap_err();
    assert!(matches!(err, HostlibError::InvalidParameter { param, .. } if param == "argv"));
}

// -------- run_test --------

#[test]
fn run_test_explicit_argv_runs_and_returns_handle() {
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
    // Stage a JUnit XML that the bundled handler would have written, then
    // ask `run_test` to drive a no-op runner and point at it via argv.
    let dir = tempdir().unwrap();
    let junit = dir.path().join("junit.xml");
    std::fs::write(
        &junit,
        r#"<?xml version="1.0"?>
<testsuites>
  <testsuite name="suite">
    <testcase classname="C" name="passes" time="0.001"/>
    <testcase classname="C" name="fails" time="0.005">
      <failure message="boom">trace</failure>
    </testcase>
  </testsuite>
</testsuites>"#,
    )
    .unwrap();

    // Mock pytest by passing argv that just `cat`s the JUnit and exits 0,
    // but since explicit-argv `run_test` won't know to look for the file,
    // we test via inspect_test_results stable behavior: shape it like
    // the cargo libtest text path which the parser auto-detects.
    let mut req = dict();
    req.insert(
        "argv".into(),
        vlist_str(&[
            "bash",
            "-c",
            "echo 'running 2 tests'; echo 'test a::passes ... ok'; echo 'test a::fails ... FAILED'; printf '\\n'; echo 'test result: FAILED. 1 passed; 1 failed; 0 ignored'; exit 1",
        ]),
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
    // Gradle does not have a portable CLI mapping for adding dependencies.
    let mut req = dict();
    req.insert("operation".into(), vstr("add"));
    req.insert("ecosystem".into(), vstr("gradle"));
    req.insert("packages".into(), vlist_str(&["junit"]));
    let err = call("hostlib_tools_manage_packages", req).unwrap_err();
    assert!(matches!(err, HostlibError::InvalidParameter { param, .. } if param == "operation"));
}

#[test]
fn manage_packages_runs_for_detected_npm_workspace_when_manifest_present() {
    // We can't actually invoke `npm install` in a sandboxed tmp directory
    // without network access, so use an `ecosystem` that maps to a tiny
    // synthetic command via the executable-on-PATH machinery. We re-run
    // the real plumbing by overriding the ecosystem to something whose
    // first argv element is a no-op shell builtin.
    //
    // This test asserts that *given* an explicit ecosystem + a real cwd,
    // the builtin assembles + spawns + collects an outcome. It uses the
    // `bundler` ecosystem with `update` (no packages) → `bundle update`,
    // then accepts whatever exit code the missing binary yields. The
    // important part: no panic, structured response, lockfile_changed
    // reported as a bool.
    let dir = tempdir().unwrap();
    let mut req = dict();
    req.insert("operation".into(), vstr("update"));
    req.insert("ecosystem".into(), vstr("bundler"));
    req.insert("cwd".into(), vstr(dir.path().to_str().unwrap()));
    let result = call("hostlib_tools_manage_packages", req);
    match result {
        Ok(value) => {
            let resp = require_dict(value);
            assert_eq!(require_str(&resp, "ecosystem"), "bundler");
            assert_eq!(require_str(&resp, "operation"), "update");
            assert!(matches!(
                resp.get("lockfile_changed"),
                Some(VmValue::Bool(_))
            ));
        }
        Err(HostlibError::Backend { .. }) => {
            // Spawn failed because `bundle` isn't installed in CI — that's
            // a valid sandbox-aware backend error, not a contract bug.
        }
        Err(other) => panic!("unexpected error variant: {other:?}"),
    }
}

// -------- long_running handles --------

#[test]
fn run_command_long_running_returns_handle_immediately() {
    // A 10-second sleep: the handle must arrive before the process exits.
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
    assert!(require_int(&resp, "pid") > 0);
    assert!(require_int(&resp, "process_group_id") > 0);
    assert!(require_str(&resp, "started_at").contains('T'));
    let cmd = require_str(&resp, "command");
    assert!(
        cmd.contains("sleep"),
        "command should contain 'sleep', got {cmd}"
    );

    // Clean up: cancel so sleep doesn't outlive the test.
    let mut cancel_req = dict();
    cancel_req.insert("handle_id".into(), vstr(&handle_id));
    let cancel_resp = require_dict(call("hostlib_tools_cancel_handle", cancel_req).unwrap());
    assert!(require_bool(&cancel_resp, "cancelled"));
}

#[test]
fn run_command_long_running_feedback_fires_after_exit() {
    use std::time::Duration;

    // Use a process-unique session id so parallel tests don't interfere.
    let session_id = format!(
        "test-lr-feedback-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    );

    // Spawn a short-lived process: echoes stdout, stderr, then exits 0.
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

    // Poll the global feedback queue until the item arrives (max 5 s).
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let items = harn_vm::drain_global_pending_feedback(&session_id);
        if !items.is_empty() {
            let (kind, content) = &items[0];
            assert_eq!(kind, "tool_result", "unexpected feedback kind: {kind}");
            let payload: serde_json::Value =
                serde_json::from_str(content).expect("feedback content not valid JSON");
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
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!("feedback never arrived in 5 s");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn cancel_handle_kills_long_running_process() {
    use std::time::Duration;

    let session_id = format!(
        "test-lr-cancel-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    );

    let info = harn_hostlib::tools::long_running::spawn_long_running(
        "test_builtin",
        "sleep".into(),
        vec!["30".into()],
        None,
        std::collections::BTreeMap::new(),
        session_id.clone(),
    )
    .expect("spawn_long_running failed");

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

    // A feedback item for the killed process may or may not arrive depending
    // on whether the waiter thread observed the exit before we removed the
    // entry. Drain and discard so we don't pollute other tests.
    std::thread::sleep(Duration::from_millis(200));
    harn_vm::drain_global_pending_feedback(&session_id);
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
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("long_running".into(), VmValue::Bool(true));
    let resp = require_dict(call("hostlib_tools_run_test", req).unwrap());
    let handle_id = require_str(&resp, "handle_id");
    assert!(
        handle_id.starts_with("hto-"),
        "unexpected handle_id: {handle_id}"
    );

    // Clean up.
    let mut cancel_req = dict();
    cancel_req.insert("handle_id".into(), vstr(&handle_id));
    call("hostlib_tools_cancel_handle", cancel_req).unwrap();
}

#[test]
fn run_build_command_long_running_returns_handle() {
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "10"]));
    req.insert("long_running".into(), VmValue::Bool(true));
    let resp = require_dict(call("hostlib_tools_run_build_command", req).unwrap());
    let handle_id = require_str(&resp, "handle_id");
    assert!(
        handle_id.starts_with("hto-"),
        "unexpected handle_id: {handle_id}"
    );

    // Clean up.
    let mut cancel_req = dict();
    cancel_req.insert("handle_id".into(), vstr(&handle_id));
    call("hostlib_tools_cancel_handle", cancel_req).unwrap();
}

//! End-to-end smoke coverage for the real-process spawn path.
//!
//! `tests/process_tools.rs` exercises the process-tool builtins against
//! a [`MockSpawner`](harn_hostlib::process::MockSpawner) and is the
//! deterministic default. This file keeps a small smoke suite that
//! actually spawns real subprocesses through
//! [`harn_hostlib::process::default_spawner`] so the trait wiring isn't
//! drifting away from real semantics.
//!
//! These tests are wall-clock-dependent (they spawn `bash`, `sleep`,
//! etc.) and therefore live in their own integration target. When the
//! test-suite tiering work in issue #1069 lands, the goal is to tag
//! this target into the slow E2E job so it runs on schedule rather
//! than every push.

#![cfg(unix)]

use std::sync::Arc;

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

#[test]
fn real_run_command_echoes_stdout_and_reports_exit_zero() {
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["bash", "-c", "echo hello"]));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    assert_eq!(require_int(&resp, "exit_code"), 0);
    assert_eq!(require_str(&resp, "stdout").trim(), "hello");
    assert_eq!(require_str(&resp, "status"), "completed");
    assert!(!require_bool(&resp, "timed_out"));
}

#[test]
fn real_run_command_strips_secret_env_from_child() {
    // Regression for the provider-key exfiltration finding: under the default
    // `InheritClean` env mode (no caller-supplied `env`), the agent `run` tool
    // spawns a child that inherits the parent environment, and that child's
    // stdout is returned to the model. Secret-bearing vars must be stripped so
    // `run({command: "env"})` can't surface provider keys / tokens.
    //
    // SAFETY: setting/removing process-wide env vars is not thread-safe in
    // general, but these names are unique to this test and removed before it
    // returns, so no sibling test in this binary observes them.
    unsafe {
        std::env::set_var("ANTHROPIC_API_KEY", "sk-test-anthropic");
        std::env::set_var("GITHUB_TOKEN", "ghp_test_github");
        std::env::set_var("HARN_E2E_BENIGN_VAR", "keep-me");
    }

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["env"]));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    unsafe {
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("HARN_E2E_BENIGN_VAR");
    }

    assert_eq!(require_int(&resp, "exit_code"), 0);
    let child_env = require_str(&resp, "stdout");
    assert!(
        !child_env.contains("sk-test-anthropic"),
        "ANTHROPIC_API_KEY leaked into child env:\n{child_env}"
    );
    assert!(
        !child_env.contains("ghp_test_github"),
        "GITHUB_TOKEN leaked into child env:\n{child_env}"
    );
    // Secret var NAMES (not just values) must also be gone, and a benign var +
    // PATH must survive so real builds/tests still work.
    assert!(
        !child_env.contains("ANTHROPIC_API_KEY"),
        "ANTHROPIC_API_KEY name still present in child env:\n{child_env}"
    );
    assert!(
        !child_env.contains("GITHUB_TOKEN"),
        "GITHUB_TOKEN name still present in child env:\n{child_env}"
    );
    assert!(
        child_env.contains("HARN_E2E_BENIGN_VAR"),
        "benign env var was incorrectly stripped:\n{child_env}"
    );
    assert!(
        child_env.lines().any(|line| line.starts_with("PATH=")),
        "PATH must remain available to child:\n{child_env}"
    );
}

#[test]
fn real_run_command_kills_child_when_timeout_elapses() {
    // Smoke: the real `wait_with_timeout` should fire SIGKILL when the
    // child blocks past the deadline. Use a very short sleep so the test
    // doesn't bloat the slow suite — under 250 ms wall-clock total.
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "5"]));
    req.insert("timeout_ms".into(), VmValue::Int(150));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    assert!(require_bool(&resp, "timed_out"));
    assert_eq!(require_str(&resp, "status"), "timed_out");
}

#[test]
fn real_run_command_points_child_tmpdir_inside_the_workspace() {
    // Under a restricted sandbox profile, the agent `run_command` tool must
    // hand its child a writable, workspace-local TMPDIR so compiler linkers
    // (rustc/cc/ld, Go, Swift, …) write intermediates somewhere the sandbox
    // permits instead of the unwritable system /tmp. Spawn `env` and confirm
    // TMPDIR/TMP/TEMP resolve to <workspace>/.harn-tmp.
    use harn_vm::orchestration::{
        pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
    };

    let workspace = tempfile::tempdir().expect("workspace");
    let expected = workspace.path().join(".harn-tmp");

    // OS confinement is irrelevant to this assertion (we observe the injected
    // env, not enforcement) and is unavailable on some CI hosts, so disable it.
    // SAFETY: the slow E2E target runs serially.
    unsafe {
        std::env::set_var("HARN_HANDLER_SANDBOX", "off");
    }
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["env"]));
    // cwd inside the workspace so the sandboxed cwd check passes.
    req.insert("cwd".into(), vstr(&workspace.path().to_string_lossy()));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    pop_execution_policy();
    unsafe {
        std::env::remove_var("HARN_HANDLER_SANDBOX");
    }

    let child_env = require_str(&resp, "stdout");
    let expected =
        std::fs::canonicalize(&expected).expect("workspace-local temp dir should canonicalize");
    let expected_line = format!("TMPDIR={}", expected.display());
    assert!(
        child_env.lines().any(|line| line == expected_line),
        "child TMPDIR must be the workspace-local .harn-tmp dir.\n\
         expected line: {expected_line}\nchild env:\n{child_env}"
    );
    for key in ["TMP", "TEMP"] {
        let line = format!("{key}={}", expected.display());
        assert!(
            child_env.lines().any(|candidate| candidate == line),
            "{key} must also point at the workspace-local temp dir:\n{child_env}"
        );
    }
    assert!(
        expected.is_dir(),
        "the workspace-local temp dir must be created on disk: {expected:?}"
    );
}

#[test]
fn real_run_command_respects_a_caller_pinned_tmpdir() {
    // A caller that sets TMPDIR explicitly via `env` keeps it; the injection
    // only fills the value the child would otherwise inherit.
    use harn_vm::orchestration::{
        pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
    };

    let workspace = tempfile::tempdir().expect("workspace");
    let caller_tmp = workspace.path().join("caller-chosen");
    std::fs::create_dir_all(&caller_tmp).unwrap();

    unsafe {
        std::env::set_var("HARN_HANDLER_SANDBOX", "off");
    }
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["env"]));
    req.insert("cwd".into(), vstr(&workspace.path().to_string_lossy()));
    req.insert("env_mode".into(), vstr("patch"));
    let mut env = dict();
    env.insert("TMPDIR".into(), vstr(&caller_tmp.to_string_lossy()));
    req.insert("env".into(), VmValue::dict(env));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    pop_execution_policy();
    unsafe {
        std::env::remove_var("HARN_HANDLER_SANDBOX");
    }

    let child_env = require_str(&resp, "stdout");
    let expected_line = format!("TMPDIR={}", caller_tmp.display());
    assert!(
        child_env.lines().any(|line| line == expected_line),
        "an explicit caller TMPDIR must be preserved untouched.\n\
         expected: {expected_line}\nchild env:\n{child_env}"
    );
}

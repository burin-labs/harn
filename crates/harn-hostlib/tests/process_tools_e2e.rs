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

use std::sync::{Arc, Mutex, MutexGuard};

use harn_hostlib::tools::ToolsCapability;
use harn_hostlib::{BuiltinRegistry, HostlibCapability, HostlibError};
use harn_vm::VmValue;

/// Serializes the tests in this binary that mutate process-wide environment
/// variables. `std::env::set_var` / `remove_var` are not thread-safe (and are
/// `unsafe` under the 2024 edition): without this lock libtest's threaded
/// runner can tear a sibling test's env read, leak a secret var across tests,
/// or, rarely, segfault. Every env-mutating test below acquires this guard and
/// holds it for its full duration.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock_env() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

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
    // This test must set the secret vars on the PARENT process so the child can
    // (attempt to) inherit them; per-`Command` `.env` wouldn't exercise the
    // strip path. SAFETY: `ENV_LOCK` is held for the whole test, so no sibling
    // env-mutating test runs concurrently, and the vars are removed before the
    // guard is released.
    let _env_guard = lock_env();
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
fn real_run_command_env_remove_strips_caller_selected_vars() {
    let _env_guard = lock_env();
    unsafe {
        std::env::set_var("HARN_E2E_REMOVE_ME", "gone");
        std::env::set_var("HARN_E2E_KEEP_ME", "kept");
    }

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["env"]));
    req.insert(
        "env_remove".into(),
        VmValue::List(Arc::new(vec![vstr("HARN_E2E_REMOVE_ME")])),
    );
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    unsafe {
        std::env::remove_var("HARN_E2E_REMOVE_ME");
        std::env::remove_var("HARN_E2E_KEEP_ME");
    }

    assert_eq!(require_int(&resp, "exit_code"), 0);
    let child_env = require_str(&resp, "stdout");
    assert!(
        !child_env.contains("HARN_E2E_REMOVE_ME="),
        "env_remove variable leaked into child env:\n{child_env}"
    );
    assert!(
        child_env.contains("HARN_E2E_KEEP_ME=kept"),
        "unrelated inherited var should survive env_remove:\n{child_env}"
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
    // SAFETY: `ENV_LOCK` is held for the whole test so no sibling env-mutating
    // test runs concurrently, and the var is removed before the guard drops.
    let _env_guard = lock_env();
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
fn real_run_command_sandbox_scope_allows_temp_cwd_outside_empty_policy_fallback() {
    use harn_vm::orchestration::{
        pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
    };

    let execution_root = tempfile::tempdir().expect("execution root");
    let command_root = tempfile::tempdir().expect("command root");
    let command_root_str = command_root.path().to_string_lossy().into_owned();

    let _env_guard = lock_env();
    unsafe {
        std::env::remove_var("HARN_PROJECT_ROOT");
        std::env::set_var("HARN_HANDLER_SANDBOX", "off");
    }
    harn_vm::stdlib::process::set_thread_execution_context(Some(
        harn_vm::orchestration::RunExecutionRecord {
            cwd: Some(execution_root.path().to_string_lossy().into_owned()),
            source_dir: None,
            env: Default::default(),
            adapter: None,
            repo_path: None,
            worktree_path: None,
            branch: None,
            base_ref: None,
            cleanup: None,
        },
    ));
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        ..CapabilityPolicy::default()
    });

    let mut sandbox = dict();
    sandbox.insert("workspace_roots".into(), vlist_str(&[&command_root_str]));
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["pwd"]));
    req.insert("cwd".into(), vstr(&command_root_str));
    req.insert("sandbox".into(), VmValue::dict(sandbox));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    pop_execution_policy();
    harn_vm::stdlib::process::set_thread_execution_context(None);
    unsafe {
        std::env::remove_var("HARN_HANDLER_SANDBOX");
    }

    assert_eq!(require_int(&resp, "exit_code"), 0);
    let stdout = require_str(&resp, "stdout");
    assert_eq!(
        stdout.trim(),
        std::fs::canonicalize(command_root.path())
            .unwrap()
            .display()
            .to_string(),
        "command should run in the scoped temp root, got stdout:\n{stdout}"
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

    // SAFETY: `ENV_LOCK` is held for the whole test so no sibling env-mutating
    // test runs concurrently, and the var is removed before the guard drops.
    let _env_guard = lock_env();
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

// --- Subprocess lifecycle: cancel/deadline interrupts kill the child group ---

/// `kill(pid, 0)` probe: returns true while the target (or, for a negative
/// pid, any member of the group) still exists.
fn unix_process_exists(pid: i64) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid as i32, 0) == 0 }
}

fn wait_for_group_death(pgid: i64, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !unix_process_exists(-pgid) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    !unix_process_exists(-pgid)
}

/// Flip an installed cancel token after `delay` from a helper thread,
/// simulating a host abort / scope cancellation firing while the foreground
/// `run_command` blocks on its child.
fn flip_after(
    cancel: &Arc<std::sync::atomic::AtomicBool>,
    delay: std::time::Duration,
) -> std::thread::JoinHandle<()> {
    let cancel = Arc::clone(cancel);
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    })
}

#[test]
fn real_run_command_interrupt_kills_the_whole_process_group() {
    // A child that spawns its own grandchild: the direct `sh` exits on
    // SIGTERM, but the backgrounded `sleep 30` must also die — that's what
    // the process-group signal is for.
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _guard = harn_vm::op_interrupt::install(Some(Arc::clone(&cancel)), None);
    let flipper = flip_after(&cancel, std::time::Duration::from_millis(300));

    let started = std::time::Instant::now();
    let mut req = dict();
    req.insert(
        "argv".into(),
        vlist_str(&["sh", "-c", "sleep 30 & echo started; wait"]),
    );
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    flipper.join().unwrap();

    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "interrupt must preempt the 30s child, took {:?}",
        started.elapsed()
    );
    assert_eq!(require_str(&resp, "status"), "killed");
    assert!(!require_bool(&resp, "timed_out"));
    assert_eq!(require_str(&resp, "stdout").trim(), "started");

    let pgid = require_int(&resp, "process_group_id");
    assert!(pgid > 0, "foreground spawn should report its process group");
    assert!(
        wait_for_group_death(pgid, std::time::Duration::from_secs(5)),
        "process group {pgid} (incl. the sleep grandchild) must be gone"
    );
}

#[test]
fn real_run_command_sigterm_immune_child_is_sigkilled_after_grace() {
    // A child that ignores SIGTERM (and keeps respawning short sleeps so the
    // shell itself is the survivor) must be SIGKILLed once the grace period
    // elapses.
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _guard = harn_vm::op_interrupt::install(Some(Arc::clone(&cancel)), None);
    let flipper = flip_after(&cancel, std::time::Duration::from_millis(100));

    let started = std::time::Instant::now();
    let mut req = dict();
    req.insert(
        "argv".into(),
        vlist_str(&["sh", "-c", "trap '' TERM; while :; do sleep 0.2; done"]),
    );
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    flipper.join().unwrap();

    let elapsed = started.elapsed();
    assert!(
        elapsed >= harn_vm::op_interrupt::SUBPROCESS_TERM_GRACE,
        "a SIGTERM-immune child should survive until the grace elapses, died after {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "SIGKILL escalation must fire shortly after the grace, took {elapsed:?}"
    );
    assert_eq!(require_str(&resp, "status"), "killed");

    let pgid = require_int(&resp, "process_group_id");
    assert!(
        wait_for_group_death(pgid, std::time::Duration::from_secs(5)),
        "process group {pgid} must be gone after SIGKILL escalation"
    );
}

#[test]
fn real_run_command_background_child_survives_interrupt() {
    // `background: true` is the fire-and-forget escape hatch: its child is
    // owned by the long-running handle store (killed via `cancel_handle` or
    // the agent-session-end hook), NOT by the invoking scope's cancellation.
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let _guard = harn_vm::op_interrupt::install(Some(cancel), None);

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "30"]));
    req.insert("background".into(), VmValue::Bool(true));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());
    assert_eq!(require_str(&resp, "status"), "running");
    let pid = require_int(&resp, "pid");
    let handle_id = require_str(&resp, "handle_id");

    // Even with the interrupt already requested, the background child stays
    // alive for a comfortable observation window.
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert!(
        unix_process_exists(pid),
        "background child {pid} must survive scope interrupts"
    );

    // Clean up so the sleep doesn't outlive the test binary.
    let mut cancel_req = dict();
    cancel_req.insert("handle_id".into(), vstr(&handle_id));
    let cancel_resp = require_dict(call("hostlib_tools_cancel_handle", cancel_req).unwrap());
    assert!(require_bool(&cancel_resp, "cancelled"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while unix_process_exists(pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(!unix_process_exists(pid), "cancel_handle must reap {pid}");
}

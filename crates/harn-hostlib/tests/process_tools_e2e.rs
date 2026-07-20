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

use std::io::{Read, Write};
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

fn vlist(values: Vec<VmValue>) -> VmValue {
    VmValue::List(Arc::new(values))
}

fn python3() -> Option<String> {
    let candidate = std::env::var("PYTHON").unwrap_or_else(|_| "python3".to_string());
    let status = std::process::Command::new(&candidate)
        .arg("-c")
        .arg("import os, sys")
        .status()
        .ok()?;
    status.success().then_some(candidate)
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

fn require_list(map: &harn_vm::value::DictMap, key: &str) -> Vec<VmValue> {
    match map.get(key) {
        Some(VmValue::List(value)) => value.as_ref().clone(),
        other => panic!("expected list at {key}, got {other:?}"),
    }
}

fn as_dict(value: &VmValue) -> harn_vm::value::DictMap {
    match value {
        VmValue::Dict(map) => (**map).clone(),
        other => panic!("expected dict value, got {other:?}"),
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
fn real_wait_command_completes_background_after_fifo_release() {
    let session_id = format!(
        "test-real-wait-command-release-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    );
    let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
    let _ = harn_vm::orchestration::agent_inbox::drain(&session_id);

    let temp = tempfile::tempdir().expect("tempdir");
    let fifo_path = temp.path().join("release.fifo");
    let mkfifo_status = std::process::Command::new("mkfifo")
        .arg(&fifo_path)
        .status()
        .expect("spawn mkfifo");
    assert!(mkfifo_status.success(), "mkfifo failed: {mkfifo_status}");
    let mut release_fifo = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo_path)
        .expect("open release fifo");

    let script = format!(
        "set -eu\nprintf 'bg-ready\\n'\nIFS= read -r _ < {}\nprintf 'bg-done\\n'\n",
        shell_words::quote(&fifo_path.to_string_lossy())
    );
    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["bash", "-c", &script]));
    req.insert("background".into(), VmValue::Bool(true));
    let start = require_dict(call("hostlib_tools_run_command", req).unwrap());
    let handle_id = require_str(&start, "handle_id");

    let (wait_started_tx, wait_started_rx) = std::sync::mpsc::sync_channel::<()>(0);
    let wait_handle_id = handle_id.clone();
    let wait_session_id = session_id.clone();
    let waiter = std::thread::spawn(move || {
        let _session_guard = harn_vm::agent_sessions::enter_current_session(wait_session_id);
        wait_started_tx
            .send(())
            .expect("wait-start receiver dropped");
        let mut wait_req = dict();
        wait_req.insert("handle_id".into(), vstr(&wait_handle_id));
        wait_req.insert("timeout_ms".into(), VmValue::Int(10_000));
        require_dict(call("hostlib_tools_wait_command", wait_req).unwrap())
    });

    wait_started_rx
        .recv()
        .expect("waiter did not start before release");
    writeln!(release_fifo, "go").expect("write release line");

    let waited = waiter.join().expect("waiter thread panicked");
    assert_eq!(require_str(&waited, "status"), "completed");
    assert_eq!(require_str(&waited, "feedback_kind"), "tool_result");
    assert_eq!(require_str(&waited, "handle_id"), handle_id);
    assert_eq!(require_int(&waited, "exit_code"), 0);
    let stdout = require_str(&waited, "stdout");
    assert!(
        stdout.contains("bg-ready\n") && stdout.contains("bg-done\n"),
        "wait_command returned before released output finalized: {stdout:?}"
    );

    let leftover = harn_vm::orchestration::agent_inbox::drain(&session_id);
    assert!(
        leftover.is_empty(),
        "explicit wait must consume matching tool_result feedback, got {leftover:?}"
    );
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
fn real_run_command_env_remove_strips_named_vars_but_explicit_env_wins() {
    // `env_remove` lets a harness strip inherited observability vars (e.g.
    // HARN_EVENT_LOG_DIR / HARN_LLM_TRANSCRIPT_DIR) so a spawned child
    // harn/burin process doesn't write into the parent's stores. An explicit
    // `env` entry for the same key must still win over the removal.
    //
    // SAFETY: `ENV_LOCK` serializes env-mutating tests; vars are removed
    // before the guard is released.
    let _env_guard = lock_env();
    unsafe {
        std::env::set_var("HARN_E2E_REMOVE_ME", "inherited-and-unwanted");
        std::env::set_var("HARN_E2E_OVERRIDE_ME", "inherited-value");
        std::env::set_var("HARN_E2E_KEEP_ME", "still-here");
    }

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["env"]));
    req.insert(
        "env_remove".into(),
        vlist_str(&["HARN_E2E_REMOVE_ME", "HARN_E2E_OVERRIDE_ME"]),
    );
    let mut env = dict();
    env.insert("HARN_E2E_OVERRIDE_ME".into(), vstr("explicit-value"));
    req.insert("env".into(), VmValue::dict(env));
    // Supplying `env` alone defaults env_mode to `replace`; force `patch` so
    // the child actually inherits the parent env this test strips from.
    req.insert("env_mode".into(), vstr("patch"));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    unsafe {
        std::env::remove_var("HARN_E2E_REMOVE_ME");
        std::env::remove_var("HARN_E2E_OVERRIDE_ME");
        std::env::remove_var("HARN_E2E_KEEP_ME");
    }

    assert_eq!(require_int(&resp, "exit_code"), 0);
    let child_env = require_str(&resp, "stdout");
    assert!(
        !child_env.contains("HARN_E2E_REMOVE_ME"),
        "env_remove'd var still present in child env:\n{child_env}"
    );
    assert!(
        child_env.contains("HARN_E2E_OVERRIDE_ME=explicit-value"),
        "explicit env override must win over env_remove:\n{child_env}"
    );
    assert!(
        !child_env.contains("inherited-value"),
        "inherited value survived despite env_remove + explicit override:\n{child_env}"
    );
    assert!(
        child_env.contains("HARN_E2E_KEEP_ME=still-here"),
        "unrelated var was incorrectly stripped:\n{child_env}"
    );
}

#[test]
fn real_run_command_cleanup_token_survives_replace_env_and_env_remove() {
    let Some(python) = python3() else {
        return;
    };

    let mut req = dict();
    req.insert(
        "argv".into(),
        vlist(vec![
            vstr(&python),
            vstr("-c"),
            vstr("import os; print(os.environ.get('HARN_PROCESS_CLEANUP_TOKEN', '<missing>'))"),
        ]),
    );
    req.insert("env_mode".into(), vstr("replace"));
    req.insert(
        "env_remove".into(),
        vlist_str(&["HARN_PROCESS_CLEANUP_TOKEN"]),
    );
    let mut env = dict();
    env.insert("HARN_PROCESS_CLEANUP_TOKEN".into(), vstr("caller-token"));
    req.insert("env".into(), VmValue::dict(env));

    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    assert_eq!(require_str(&resp, "status"), "completed");
    let stdout = require_str(&resp, "stdout");
    assert!(
        stdout.trim().starts_with("harn-cleanup-"),
        "private cleanup token should be injected after env_clear/env_remove/env overrides, got: {stdout:?}"
    );
    assert_ne!(stdout.trim(), "caller-token");
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
fn real_run_command_file_capture_kills_child_when_timeout_elapses() {
    let mut capture: harn_vm::value::DictMap = Default::default();
    capture.insert("transport".into(), vstr("file"));

    let mut req = dict();
    req.insert("argv".into(), vlist_str(&["sleep", "5"]));
    req.insert("timeout_ms".into(), VmValue::Int(150));
    req.insert("capture".into(), VmValue::dict(capture));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    assert!(require_bool(&resp, "timed_out"));
    assert_eq!(require_str(&resp, "status"), "timed_out");
    assert_eq!(require_int(&resp, "exit_code"), -1);
    assert_eq!(require_str(&resp, "signal"), "SIGKILL");
    let pid = require_int(&resp, "pid");
    let cleanup = require_nested_dict(&resp, "process_cleanup");
    assert_eq!(require_int(&cleanup, "root_pid"), pid);
    let pgid = require_int(&resp, "process_group_id");
    assert!(
        !unix_process_exists(-pgid),
        "timed-out file-capture process group {pgid} must be gone"
    );
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
            project_root: None,
            source_dir: None,
            env: Default::default(),
            adapter: None,
            repo_path: None,
            worktree_path: None,
            branch: None,
            base_ref: None,
            cleanup: None,
            grants: Vec::new(),
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

fn unix_kill_process(pid: i64) {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe {
        let _ = kill(pid as i32, 9);
    }
}

#[test]
fn real_run_command_interrupt_kills_the_whole_process_group() {
    // A child that spawns its own grandchild: the direct `sh` exits on
    // SIGTERM, but the backgrounded `sleep 30` must also die — that's what
    // the process-group signal is for.
    let temp = tempfile::tempdir().expect("ready fifo tempdir");
    let ready_path = temp.path().join("ready.fifo");
    let status = std::process::Command::new("mkfifo")
        .arg(&ready_path)
        .status()
        .expect("spawn mkfifo");
    assert!(status.success(), "mkfifo failed: {status}");
    let mut ready_fifo = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&ready_path)
        .expect("open ready fifo");

    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let ready_path_arg = shell_words::quote(&ready_path.to_string_lossy()).into_owned();
    let worker = std::thread::spawn(move || {
        let _guard = harn_vm::op_interrupt::install(Some(worker_cancel), None);
        let mut req = dict();
        req.insert(
            "argv".into(),
            vlist(vec![
                vstr("sh"),
                vstr("-c"),
                vstr(&format!("sleep 30 & printf ready > {ready_path_arg}; wait")),
            ]),
        );
        require_dict(call("hostlib_tools_run_command", req).unwrap())
    });

    let mut marker = [0_u8; 5];
    ready_fifo
        .read_exact(&mut marker)
        .expect("child did not signal grandchild readiness");
    assert_eq!(&marker, b"ready");
    cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    let resp = worker
        .join()
        .expect("interruptible command thread panicked");

    assert_eq!(require_str(&resp, "status"), "killed");
    assert!(!require_bool(&resp, "timed_out"));

    let pgid = require_int(&resp, "process_group_id");
    assert!(pgid > 0, "foreground spawn should report its process group");
    let cleanup = require_nested_dict(&resp, "process_cleanup");
    assert!(
        require_int(&cleanup, "observed_child_count") >= 1,
        "cleanup receipt should record the background sleep descendant"
    );
    assert_eq!(require_int(&cleanup, "survivor_count"), 0);
    assert!(
        !unix_process_exists(-pgid),
        "process group {pgid} (incl. the sleep grandchild) must be gone"
    );
}

#[test]
fn real_run_command_sigterm_immune_child_is_sigkilled_after_grace() {
    // A child that ignores SIGTERM must be SIGKILLed once the grace period
    // elapses. Keep the fixture to one process so cleanup observes a stable
    // process group while it performs its final survivor sweep.
    let temp = tempfile::tempdir().expect("ready fifo tempdir");
    let ready_path = temp.path().join("ready.fifo");
    let status = std::process::Command::new("mkfifo")
        .arg(&ready_path)
        .status()
        .expect("spawn mkfifo");
    assert!(status.success(), "mkfifo failed: {status}");
    let mut ready_fifo = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&ready_path)
        .expect("open ready fifo");

    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let ready_path_arg = shell_words::quote(&ready_path.to_string_lossy()).into_owned();
    let worker = std::thread::spawn(move || {
        let _guard = harn_vm::op_interrupt::install(Some(worker_cancel), None);
        let mut req = dict();
        req.insert(
            "argv".into(),
            vlist(vec![
                vstr("sh"),
                vstr("-c"),
                vstr(&format!(
                    "trap '' TERM; printf ready > {ready_path_arg}; exec sleep 30"
                )),
            ]),
        );
        require_dict(call("hostlib_tools_run_command", req).unwrap())
    });

    let mut marker = [0_u8; 5];
    ready_fifo
        .read_exact(&mut marker)
        .expect("SIGTERM-immune child did not signal readiness");
    assert_eq!(&marker, b"ready");
    cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    let resp = worker
        .join()
        .expect("interruptible command thread panicked");

    assert_eq!(require_str(&resp, "status"), "killed");

    let pgid = require_int(&resp, "process_group_id");
    assert!(pgid > 0);
    let cleanup = require_nested_dict(&resp, "process_cleanup");
    let signals = require_list(&cleanup, "attempted_signals");
    assert!(signals.iter().any(|value| matches!(
        value,
        VmValue::String(value) if value.as_str() == "SIGTERM"
    )));
    assert!(signals.iter().any(|value| matches!(
        value,
        VmValue::String(value) if value.as_str() == "SIGKILL"
    )));
    assert_eq!(require_int(&cleanup, "survivor_count"), 0);
    assert!(!unix_process_exists(-pgid));
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

    // Spawn returns only after the OS has assigned the child a PID. A direct
    // liveness probe is enough here; sleeping to manufacture an observation
    // window makes this smoke test sensitive to host load.
    assert!(
        unix_process_exists(pid),
        "background child {pid} must survive scope interrupts"
    );

    // Clean up through the same event-driven completion signal used by the
    // deterministic mock-based lifecycle tests.
    let completion_rx = harn_hostlib::tools::long_running::register_completion_notifier(&handle_id)
        .expect("background handle should still be live");
    let mut cancel_req = dict();
    cancel_req.insert("handle_id".into(), vstr(&handle_id));
    let cancel_resp = require_dict(call("hostlib_tools_cancel_handle", cancel_req).unwrap());
    assert!(require_bool(&cancel_resp, "cancelled"));
    completion_rx
        .recv()
        .expect("background waiter did not publish completion");
    assert!(!unix_process_exists(pid), "cancel_handle must reap {pid}");
}

struct PidFileCleanup {
    path: std::path::PathBuf,
}

impl Drop for PidFileCleanup {
    fn drop(&mut self) {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return;
        };
        let Ok(pid) = raw.trim().parse::<i64>() else {
            return;
        };
        if pid > 1 && unix_process_exists(pid) {
            unix_kill_process(pid);
        }
    }
}

#[test]
fn real_run_command_token_cleanup_reaps_reparented_pipe_holder() {
    let Some(python) = python3() else {
        return;
    };
    let temp = tempfile::tempdir().expect("pid tempdir");
    let pid_path = temp.path().join("descendant.pid");
    let _cleanup_guard = PidFileCleanup {
        path: pid_path.clone(),
    };
    let pid_path_arg = pid_path.to_string_lossy().to_string();
    let parent = r#"
import pathlib
import subprocess
import sys

pid_path = sys.argv[1]
child = "import signal; signal.pause()"
descendant = subprocess.Popen([sys.executable, "-c", child], start_new_session=True)
pathlib.Path(pid_path).write_text(str(descendant.pid))
print("parent-exit", flush=True)
"#;

    let mut req = dict();
    req.insert(
        "argv".into(),
        vlist(vec![
            vstr(&python),
            vstr("-c"),
            vstr(parent),
            vstr(&pid_path_arg),
        ]),
    );
    req.insert("timeout_ms".into(), VmValue::Int(500));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    assert!(require_bool(&resp, "timed_out"));
    assert_eq!(require_str(&resp, "status"), "timed_out");
    let stdout = require_str(&resp, "stdout");
    assert!(
        stdout.contains("parent-exit"),
        "stdout should preserve the direct parent's marker before cleanup: {stdout:?}"
    );
    let raw_descendant_pid =
        std::fs::read_to_string(&pid_path).expect("descendant pid should be recorded");
    let descendant_pid = raw_descendant_pid
        .trim()
        .parse::<i64>()
        .expect("descendant pid");
    let cleanup = require_nested_dict(&resp, "process_cleanup");
    assert!(
        require_int(&cleanup, "observed_child_count") >= 1,
        "cleanup receipt should include the reparented same-token descendant: {cleanup:?}"
    );
    assert!(
        require_int(&cleanup, "reaped_child_count") >= 1,
        "same-token descendant should be reaped: {cleanup:?}"
    );
    assert_eq!(require_int(&cleanup, "survivor_count"), 0);
    let observed_children = require_list(&cleanup, "observed_children");
    let observed_descendant = observed_children
        .iter()
        .map(as_dict)
        .find(|child| require_int(child, "pid") == descendant_pid)
        .unwrap_or_else(|| {
            panic!(
                "cleanup receipt should observe exact escaped descendant pid {descendant_pid}: {cleanup:?}"
            )
        });
    assert!(
        observed_descendant.get("command").is_none(),
        "cleanup receipt must not include raw child command text: {observed_descendant:?}"
    );
    let reaped_children = require_list(&cleanup, "reaped_children");
    let reaped_descendant = reaped_children
        .iter()
        .map(as_dict)
        .find(|child| require_int(child, "pid") == descendant_pid)
        .unwrap_or_else(|| {
            panic!(
                "cleanup receipt should reap exact escaped descendant pid {descendant_pid}: {cleanup:?}"
            )
        });
    assert!(
        reaped_descendant.get("command").is_none(),
        "cleanup receipt must not include raw reaped child command text: {reaped_descendant:?}"
    );
    assert!(
        !unix_process_exists(descendant_pid),
        "reparented descendant {descendant_pid} must be gone after token cleanup"
    );
}

#[test]
fn real_run_command_file_capture_does_not_wait_for_reparented_pipe_holder() {
    let Some(python) = python3() else {
        return;
    };
    let temp = tempfile::tempdir().expect("pid tempdir");
    let pid_path = temp.path().join("descendant.pid");
    let script_path = temp.path().join("parent.py");
    let _cleanup_guard = PidFileCleanup {
        path: pid_path.clone(),
    };
    let parent = r#"
import pathlib
import subprocess
import sys

pid_path = sys.argv[1]
child = "import signal; signal.pause()"
descendant = subprocess.Popen([sys.executable, "-c", child], start_new_session=True)
pathlib.Path(pid_path).write_text(str(descendant.pid))
print("parent-exit", flush=True)
"#;
    std::fs::write(&script_path, parent).expect("write parent script");

    let mut capture: harn_vm::value::DictMap = Default::default();
    capture.insert("transport".into(), vstr("file"));
    let mut req = dict();
    let command = format!(
        "{} {} {}",
        shell_words::quote(&python),
        shell_words::quote(&script_path.to_string_lossy()),
        shell_words::quote(&pid_path.to_string_lossy())
    );
    req.insert("mode".into(), vstr("shell"));
    req.insert("command".into(), vstr(&command));
    req.insert("shell_id".into(), vstr("sh"));
    req.insert("timeout_ms".into(), VmValue::Int(500));
    req.insert("capture".into(), VmValue::dict(capture));
    let resp = require_dict(call("hostlib_tools_run_command", req).unwrap());

    assert!(!require_bool(&resp, "timed_out"));
    assert_eq!(require_str(&resp, "status"), "completed");
    assert_eq!(require_int(&resp, "exit_code"), 0);
    let stdout = require_str(&resp, "stdout");
    assert!(
        stdout.contains("parent-exit"),
        "file capture should preserve direct-run output: {stdout:?}"
    );
    assert!(resp.get("process_cleanup").is_none());
}

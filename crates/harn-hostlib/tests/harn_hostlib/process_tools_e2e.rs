//! End-to-end smoke coverage for the real-process spawn path.
//!
//! `tests/harn_hostlib/process_tools.rs` exercises the process-tool builtins against
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

use std::io::{BufRead, Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
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
    let _guardian_args = harn_hostlib::process::owner_death::install_guardian_reexec_args([
        "--exact",
        "process_tools_e2e::owner_death_guardian_fixture",
        "--nocapture",
    ]);
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

const OWNER_DEATH_SUPERVISOR_ENV: &str = "HARN_TEST_OWNER_DEATH_SUPERVISOR";
const OWNER_DEATH_REPORT_FD_ENV: &str = "HARN_TEST_OWNER_DEATH_REPORT_FD";

#[test]
fn owner_death_guardian_fixture() {
    if !harn_hostlib::process::owner_death::guardian_requested() {
        return;
    }
    harn_hostlib::process::owner_death::run_guardian_from_env().expect("run owner-death guardian");
}

#[test]
fn owner_death_grandchild_fixture() {
    if std::env::var_os(OWNER_DEATH_REPORT_FD_ENV).is_none() {
        return;
    }
    loop {
        unsafe {
            libc::pause();
        }
    }
}

#[test]
fn owner_death_payload_fixture() {
    let Some(report_fd) = std::env::var(OWNER_DEATH_REPORT_FD_ENV)
        .ok()
        .and_then(|value| value.parse::<RawFd>().ok())
    else {
        return;
    };
    let grandchild = std::process::Command::new(
        std::env::current_exe().expect("resolve process-tools test executable"),
    )
    .args([
        "--exact",
        "process_tools_e2e::owner_death_grandchild_fixture",
        "--nocapture",
    ])
    .spawn()
    .expect("spawn native grandchild fixture");
    let report = format!(
        "payload={} pgid={} grandchild={}\n",
        std::process::id(),
        unsafe { libc::getpgrp() },
        grandchild.id()
    );
    let written = unsafe { libc::write(report_fd, report.as_ptr().cast(), report.len()) };
    assert_eq!(written, report.len() as isize, "write payload handshake");
    std::mem::forget(grandchild);
    loop {
        unsafe {
            libc::pause();
        }
    }
}

#[test]
fn owner_death_supervisor_fixture() {
    if std::env::var_os(OWNER_DEATH_SUPERVISOR_ENV).is_none() {
        return;
    }
    let _guardian_args = harn_hostlib::process::owner_death::install_guardian_reexec_args([
        "--exact",
        "process_tools_e2e::owner_death_guardian_fixture",
        "--nocapture",
    ]);
    let info = harn_hostlib::tools::long_running::spawn_long_running(
        "owner_death_supervisor_fixture",
        std::env::current_exe()
            .expect("resolve process-tools test executable")
            .to_string_lossy()
            .into_owned(),
        vec![
            "--exact".to_string(),
            "process_tools_e2e::owner_death_payload_fixture".to_string(),
            "--nocapture".to_string(),
        ],
        None,
        std::collections::BTreeMap::new(),
        format!("owner-death-supervisor-{}", std::process::id()),
    )
    .expect("spawn managed background payload");
    println!(
        "supervisor={} supervisor_pgid={} worker={} worker_pgid={}",
        std::process::id(),
        unsafe { libc::getpgrp() },
        info.pid,
        info.process_group_id.expect("worker process group")
    );
    std::io::stdout().flush().expect("flush supervisor report");
    loop {
        unsafe {
            libc::pause();
        }
    }
}

struct ProcessGroupCleanup {
    groups: Vec<i32>,
}

impl Drop for ProcessGroupCleanup {
    fn drop(&mut self) {
        for pgid in &self.groups {
            if *pgid > 0 {
                unsafe {
                    libc::kill(-*pgid, libc::SIGKILL);
                }
            }
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn managed_background_group_dies_when_its_supervisor_is_sigkilled() {
    let mut report_pipe = [0_i32; 2];
    assert_eq!(unsafe { libc::pipe(report_pipe.as_mut_ptr()) }, 0);
    let read_fd = report_pipe[0];
    let write_fd = report_pipe[1];
    let read_flags = unsafe { libc::fcntl(read_fd, libc::F_GETFD) };
    assert!(read_flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(read_fd, libc::F_SETFD, read_flags | libc::FD_CLOEXEC) },
        0
    );

    let mut supervisor = std::process::Command::new(
        std::env::current_exe().expect("resolve process-tools test executable"),
    );
    supervisor
        .args([
            "--exact",
            "process_tools_e2e::owner_death_supervisor_fixture",
            "--nocapture",
        ])
        .env(OWNER_DEATH_SUPERVISOR_ENV, "1")
        .env(OWNER_DEATH_REPORT_FD_ENV, write_fd.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .process_group(0);
    let mut supervisor = supervisor.spawn().expect("spawn isolated supervisor");
    unsafe {
        libc::close(write_fd);
    }
    let mut cleanup = ProcessGroupCleanup {
        groups: vec![supervisor.id() as i32],
    };

    let supervisor_stdout = supervisor.stdout.take().expect("supervisor stdout");
    let mut supervisor_lines = std::io::BufReader::new(supervisor_stdout).lines();
    let supervisor_report = supervisor_lines
        .find_map(|line| {
            let line = line.expect("read supervisor report");
            line.starts_with("supervisor=").then_some(line)
        })
        .expect("supervisor report line");
    let fields = parse_pid_fields(&supervisor_report);
    let supervisor_pid = fields["supervisor"];
    let supervisor_pgid = fields["supervisor_pgid"];
    let worker_pid = fields["worker"];
    let worker_pgid = fields["worker_pgid"];
    assert_eq!(supervisor_pid, supervisor_pgid);
    assert_eq!(worker_pid, worker_pgid);
    assert_ne!(worker_pgid, supervisor_pgid);
    cleanup.groups.push(worker_pgid);

    let mut report_file = unsafe { std::fs::File::from_raw_fd(read_fd) };
    let mut payload_report = String::new();
    std::io::BufReader::new(&mut report_file)
        .read_line(&mut payload_report)
        .expect("read payload report");
    let payload_fields = parse_pid_fields(&payload_report);
    assert_eq!(payload_fields["pgid"], worker_pgid);
    assert_ne!(payload_fields["payload"], payload_fields["grandchild"]);

    assert_eq!(
        unsafe { libc::kill(-supervisor_pgid, libc::SIGKILL) },
        0,
        "SIGKILL isolated supervisor group"
    );
    supervisor.wait().expect("reap supervisor");

    let mut poll_fd = libc::pollfd {
        fd: read_fd,
        events: libc::POLLIN | libc::POLLHUP,
        revents: 0,
    };
    assert_eq!(
        unsafe { libc::poll(&raw mut poll_fd, 1, 10_000) },
        1,
        "worker descriptors did not close after owner death"
    );
    let mut eof = [0_u8; 1];
    assert_eq!(
        report_file.read(&mut eof).expect("read owner-death EOF"),
        0,
        "worker report descriptor remained open"
    );
    wait_for_native_exit(worker_pid);
    if unsafe { libc::kill(-worker_pgid, 0) } != -1
        || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    {
        // Darwin can report the just-exited group as EAGAIN between NOTE_EXIT
        // delivery and launchd reaping its orphaned leader. A second native
        // process-exit barrier closes that kernel transition without sleeping
        // or polling.
        wait_for_native_exit(worker_pid);
    }
    assert_eq!(unsafe { libc::kill(-worker_pgid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    cleanup.groups.clear();
}

fn parse_pid_fields(line: &str) -> std::collections::BTreeMap<&str, i32> {
    line.split_whitespace()
        .filter_map(|field| field.split_once('='))
        .map(|(key, value)| {
            (
                key,
                value
                    .parse::<i32>()
                    .unwrap_or_else(|_| panic!("invalid pid field {key}={value}")),
            )
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn wait_for_native_exit(pid: i32) {
    let pid_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
    if pid_fd < 0 {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        return;
    }
    let mut poll_fd = libc::pollfd {
        fd: pid_fd,
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&raw mut poll_fd, 1, 10_000) }, 1);
    unsafe {
        libc::close(pid_fd);
    }
}

#[cfg(target_os = "macos")]
fn wait_for_native_exit(pid: i32) {
    let queue = unsafe { libc::kqueue() };
    assert!(queue >= 0, "create process kqueue");
    let change = libc::kevent {
        ident: pid as usize,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    let timeout = libc::timespec {
        tv_sec: 10,
        tv_nsec: 0,
    };
    let mut event = change;
    let result = unsafe {
        libc::kevent(
            queue,
            &raw const change,
            1,
            &raw mut event,
            1,
            &raw const timeout,
        )
    };
    unsafe {
        libc::close(queue);
    }
    if result < 0 {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    } else {
        assert_eq!(result, 1, "worker did not exit before kernel deadline");
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
fn real_background_spawn_failure_is_reported_before_returning_a_handle() {
    let mut req = dict();
    req.insert(
        "argv".into(),
        vlist_str(&["harn-owner-death-command-that-does-not-exist"]),
    );
    req.insert("background".into(), VmValue::Bool(true));
    let error = call("hostlib_tools_run_command", req).expect_err("background spawn must fail");
    assert!(
        error
            .to_string()
            .contains("harn-owner-death-command-that-does-not-exist")
            || error.to_string().contains("No such file"),
        "unexpected background spawn error: {error}"
    );
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
    assert_process_gone(
        -pgid,
        &format!("timed-out file-capture process group {pgid}"),
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
/// pid, any member of the group) still exists. A killed-but-unreaped zombie
/// still counts as existing, so a single negative probe cannot prove a process
/// is gone — see [`assert_process_gone`].
fn unix_process_exists(pid: i64) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid as i32, 0) == 0 }
}

/// Assert a pid (or, for a negative pid, a whole process group) has vanished,
/// tolerating reap latency.
///
/// Signalling a process is synchronous but *reaping* it is not: between the
/// kill and the parent's (or init's) `wait`, the pid lingers as a zombie and
/// `kill(pid, 0)` keeps succeeding. There is no observable event a test can
/// synchronize on for that transition, so this bounded poll is the honest
/// mechanism — it is not a load-sensitive sleep standing in for a missing
/// signal. A process that is genuinely still alive never vanishes, so the
/// assertion still fails; the window only absorbs scheduling delay on a loaded
/// CI host.
fn assert_process_gone(pid: i64, what: &str) {
    const REAP_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

    let deadline = std::time::Instant::now() + REAP_WINDOW;
    while unix_process_exists(pid) {
        assert!(
            std::time::Instant::now() < deadline,
            "{what} still exists after waiting {REAP_WINDOW:?} for it to be reaped"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
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
    assert_process_gone(
        -pgid,
        &format!("process group {pgid} (incl. the sleep grandchild)"),
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
    assert_process_gone(-pgid, &format!("SIGKILLed process group {pgid}"));
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

    // The spawn response is the synchronization point: the OS has assigned a
    // PID and the long-running session store owns the child, not the
    // interrupted invoking scope. A direct liveness probe avoids a
    // load-sensitive sleep used only to manufacture an observation window.
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
    assert_process_gone(
        descendant_pid,
        &format!("reparented descendant {descendant_pid} after token cleanup"),
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

#[test]
#[should_panic(expected = "still exists after waiting")]
fn assert_process_gone_still_fails_on_a_genuinely_live_process() {
    // Guards the reap-tolerance window in `assert_process_gone` against being
    // widened into vacuity: a process that never exits must still trip the
    // assertion rather than being absorbed as reap latency.
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn live child");
    let pid = child.id() as i64;
    let result = std::panic::catch_unwind(|| assert_process_gone(pid, "live child"));
    let _ = child.kill();
    let _ = child.wait();
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

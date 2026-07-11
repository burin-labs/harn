//! Non-blocking process spawn registry for `host_call("process", "spawn"|...)`.
//!
//! This module backs the non-blocking sibling of the synchronous
//! `process.exec` capability. A `spawn` call builds the child through the
//! SAME sandbox-gated command builder as `exec`
//! ([`crate::stdlib::host::build_sandboxed_command`]) and the SAME command
//! policy preflight, then returns a handle dict **immediately** instead of
//! awaiting completion. A detached `tokio::spawn` task drains stdout/stderr
//! into shared buffers and records the final exit status. Callers observe
//! progress via:
//!
//! - `poll(handle_id)` — non-blocking snapshot (status + captured output).
//! - `wait(handle_id, timeout_ms?)` — await completion (leaves the process
//!   running if the wait times out; does NOT kill on wait-timeout).
//! - `kill(handle_id)` — signal-kill and await the status transition.
//! - `release(handle_id)` — drop the entry and free retained output.
//!
//! ### Leak safety
//!
//! The registry is capped (see [`REGISTRY_CAP`]). When a new spawn would
//! exceed the cap, the OLDEST *terminal* (exited/killed/timed_out) entries
//! are evicted first; a `Running` entry is never silently dropped. As a
//! defensive backstop, if eviction ever had to drop a still-running entry
//! it kills the child first. `kill_on_drop(true)` on the command is a
//! second backstop so a dropped `Child` does not outlive the registry.

use crate::value::VmDictExt;
use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::sync::Notify;

use crate::value::{VmError, VmValue};

use super::host::{
    audited_utc_now_rfc3339, build_sandboxed_command, optional_i64, optional_string,
    push_sandbox_profile_override, require_param,
};

/// Maximum number of spawned-process entries retained at once. Terminal
/// entries are evicted oldest-first to stay at or below this cap.
const REGISTRY_CAP: usize = 64;

static HANDLE_COUNTER: AtomicU64 = AtomicU64::new(1);

static SPAWN_REGISTRY: LazyLock<Mutex<BTreeMap<String, Arc<SpawnEntry>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// Lifecycle status of a spawned process.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SpawnStatus {
    Running,
    Exited,
    Killed,
    TimedOut,
}

impl SpawnStatus {
    fn as_str(self) -> &'static str {
        match self {
            SpawnStatus::Running => "running",
            SpawnStatus::Exited => "exited",
            SpawnStatus::Killed => "killed",
            SpawnStatus::TimedOut => "timed_out",
        }
    }

    fn is_terminal(self) -> bool {
        !matches!(self, SpawnStatus::Running)
    }
}

/// Mutable state shared between the registry entry and its detached
/// drain/wait task.
#[derive(Default)]
struct SpawnState {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_code: Option<i32>,
    status: Option<SpawnStatus>,
    ended_at: Option<String>,
}

/// A single spawned process tracked in the registry.
struct SpawnEntry {
    handle_id: String,
    pid: Option<u32>,
    cleanup_token: String,
    owner_key: Option<usize>,
    cleanup_registration: Mutex<Option<crate::op_interrupt::ActiveProcessCleanupGuard>>,
    command_display: String,
    started_at: String,
    /// Monotonic registration sequence — used for oldest-first eviction.
    seq: u64,
    state: Mutex<SpawnState>,
    /// Notified once when the process reaches a terminal status.
    completion: Notify,
    /// Notified to request a kill of the running child.
    kill_signal: Notify,
}

impl SpawnEntry {
    fn unregister_cleanup(&self) {
        let _ = self
            .cleanup_registration
            .lock()
            .expect("spawn cleanup registration poisoned")
            .take();
    }

    fn current_status(&self) -> SpawnStatus {
        self.state
            .lock()
            .expect("spawn state poisoned")
            .status
            .unwrap_or(SpawnStatus::Running)
    }

    /// Record a terminal status, the first writer winning, and return the
    /// effective terminal status. A `kill()` request and the detached reaper
    /// can both reach a terminal state concurrently; whichever records first
    /// fixes the outcome (a process killed at the same instant it exits
    /// naturally stays `killed`, matching the caller's intent), and the loser
    /// becomes a no-op that still reads back the winning status. `exit_code`
    /// and `ended_at` are stamped only on the first write.
    fn record_terminal(&self, status: SpawnStatus, exit_code: i32) -> SpawnStatus {
        let mut state = self.state.lock().expect("spawn state poisoned");
        match state.status {
            Some(existing) if existing.is_terminal() => existing,
            _ => {
                state.status = Some(status);
                state.exit_code = Some(exit_code);
                state.ended_at = Some(audited_utc_now_rfc3339("host_call/process.spawn.ended_at"));
                status
            }
        }
    }
}

/// Allocate a monotonic sequence number and the handle id derived from it.
/// The same `seq` is used for oldest-first eviction so ids and ordering
/// stay consistent from a single counter bump.
fn next_handle(pid: Option<u32>) -> (String, u64) {
    let n = HANDLE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = pid.unwrap_or(0);
    (format!("psh-{pid:x}-{n}"), n)
}

fn unknown_handle_error(handle_id: &str) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
        "host_call process: unknown spawn handle {handle_id:?}"
    ))))
}

fn lookup(handle_id: &str) -> Result<Arc<SpawnEntry>, VmError> {
    SPAWN_REGISTRY
        .lock()
        .expect("spawn registry poisoned")
        .get(handle_id)
        .cloned()
        .ok_or_else(|| unknown_handle_error(handle_id))
}

/// Evict oldest terminal entries to keep the registry at or below the cap.
/// Never evicts a `Running` entry under normal flow; the defensive branch
/// kills a running child if it is ever forced out (should not happen).
fn evict_if_needed(registry: &mut BTreeMap<String, Arc<SpawnEntry>>, owner_key: Option<usize>) {
    while registry.len() >= REGISTRY_CAP {
        // Prefer the oldest terminal entry in the same owner scope. A busy
        // run should not evict another run's still-observable terminal
        // handles while it has its own terminal entries available.
        let victim = registry
            .values()
            .filter(|e| e.owner_key == owner_key && e.current_status().is_terminal())
            .min_by_key(|e| e.seq)
            .or_else(|| {
                registry
                    .values()
                    .filter(|e| e.current_status().is_terminal())
                    .min_by_key(|e| e.seq)
            })
            .map(|e| e.handle_id.clone());
        let victim = match victim {
            Some(handle_id) => handle_id,
            None => {
                // No terminal entries: every slot is running. Evict the
                // oldest running entry as a hard backstop and kill it so we
                // never leak the child. This is not expected to happen
                // under the command policy (which bounds concurrency), so
                // log it loudly.
                match registry.values().min_by_key(|e| e.seq) {
                    Some(entry) => {
                        tracing::warn!(
                            handle_id = %entry.handle_id,
                            "process.spawn registry over cap with no terminal entries; \
                             killing oldest running handle to evict"
                        );
                        entry.kill_signal.notify_one();
                        entry.handle_id.clone()
                    }
                    None => return,
                }
            }
        };
        if let Some(entry) = registry.remove(&victim) {
            signal_entry_process_tree(&entry);
            tracing::info!(
                handle_id = %entry.handle_id,
                cap = REGISTRY_CAP,
                "process.spawn evicted handle (registry at cap)"
            );
        }
    }
}

/// Dispatch the non-blocking process operations. Returns `Some(result)` for
/// the operations this module owns, `None` for anything else so the caller
/// can fall through.
pub(crate) async fn dispatch(
    operation: &str,
    params: &crate::value::DictMap,
    owner_cancel_token: Option<Arc<AtomicBool>>,
) -> Option<Result<VmValue, VmError>> {
    match operation {
        "spawn" => Some(spawn(params, owner_cancel_token).await),
        "poll" => Some(poll(params)),
        "wait" => Some(wait(params).await),
        "kill" => Some(kill(params).await),
        "release" => Some(release(params)),
        _ => None,
    }
}

async fn spawn(
    params: &crate::value::DictMap,
    owner_cancel_token: Option<Arc<AtomicBool>>,
) -> Result<VmValue, VmError> {
    let timeout_ms = optional_i64(params, "timeout")
        .or_else(|| optional_i64(params, "timeout_ms"))
        .filter(|value| *value > 0)
        .map(|value| value as u64);

    // Honor an optional per-call sandbox_profile override identically to
    // process.exec — the guard must be live across the command build.
    let profile_guard = match optional_string(params, "sandbox_profile") {
        Some(value) => Some(push_sandbox_profile_override(&value)?),
        None => None,
    };
    let mut cmd = build_sandboxed_command(params, "process.spawn")?;
    crate::op_interrupt::configure_tokio_kill_group(&mut cmd);
    let cleanup_token = crate::op_interrupt::new_process_cleanup_token();
    cmd.env(
        crate::op_interrupt::PROCESS_CLEANUP_TOKEN_ENV,
        &cleanup_token,
    );
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let command_display = command_display(params);
    let started_at = audited_utc_now_rfc3339("host_call/process.spawn.started_at");
    let mut child = cmd
        .spawn()
        .map_err(|e| VmError::Runtime(format!("host_call process.spawn: {e}")))?;
    drop(profile_guard);

    let pid = child.id();
    let (handle_id, seq) = next_handle(pid);

    let owner_key = owner_cancel_token
        .as_ref()
        .map(|token| Arc::as_ptr(token) as usize);

    let entry = Arc::new(SpawnEntry {
        handle_id: handle_id.clone(),
        pid,
        cleanup_token: cleanup_token.clone(),
        owner_key,
        cleanup_registration: Mutex::new(Some(
            crate::op_interrupt::register_active_process_cleanup(
                pid,
                &cleanup_token,
                owner_cancel_token,
            ),
        )),
        command_display: command_display.clone(),
        started_at: started_at.clone(),
        seq,
        state: Mutex::new(SpawnState::default()),
        completion: Notify::new(),
        kill_signal: Notify::new(),
    });

    // Take the piped readers before moving `child` into the wait task.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let task_entry = Arc::clone(&entry);
    tokio::spawn(async move {
        run_to_completion(child, stdout_pipe, stderr_pipe, timeout_ms, task_entry).await;
    });

    {
        let mut registry = SPAWN_REGISTRY.lock().expect("spawn registry poisoned");
        evict_if_needed(&mut registry, owner_key);
        registry.insert(handle_id.clone(), entry);
    }

    let mut result = BTreeMap::new();
    result.put_str("handle_id", handle_id);
    result.insert(
        "pid".to_string(),
        pid.map(|p| VmValue::Int(p as i64)).unwrap_or(VmValue::Nil),
    );
    result.put_str("started_at", started_at);
    result.put_str("command", command_display);
    result.put_str("status", "running");
    Ok(VmValue::dict(result))
}

/// Detached task: drain stdout/stderr, await exit vs kill vs timeout, then
/// record the terminal state and notify completion.
async fn run_to_completion(
    mut child: tokio::process::Child,
    stdout_pipe: Option<tokio::process::ChildStdout>,
    stderr_pipe: Option<tokio::process::ChildStderr>,
    timeout_ms: Option<u64>,
    entry: Arc<SpawnEntry>,
) {
    let stdout_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr_buf = Arc::new(Mutex::new(Vec::<u8>::new()));

    let stdout_task = stdout_pipe.map(|pipe| {
        let buf = Arc::clone(&stdout_buf);
        tokio::spawn(drain_into(pipe, buf))
    });
    let stderr_task = stderr_pipe.map(|pipe| {
        let buf = Arc::clone(&stderr_buf);
        tokio::spawn(drain_into(pipe, buf))
    });

    // Race the child's natural exit against a kill request and (optionally)
    // a timeout. `Outcome::Terminate` carries the status to record after we
    // actively kill the child — we must release the `child.wait()` borrow
    // before re-borrowing `child` to kill it, so the select only *decides*
    // what to do and the kill happens afterward.
    enum Outcome {
        Exited(std::io::Result<std::process::ExitStatus>),
        Terminate(SpawnStatus),
    }
    let mut timeout_sleep =
        timeout_ms.map(|ms| Box::pin(tokio::time::sleep(Duration::from_millis(ms))));
    let outcome = {
        let wait = child.wait();
        tokio::pin!(wait);
        if let Some(sleep) = timeout_sleep.as_mut() {
            tokio::select! {
                result = &mut wait => Outcome::Exited(result),
                _ = entry.kill_signal.notified() => Outcome::Terminate(SpawnStatus::Killed),
                _ = sleep.as_mut() => Outcome::Terminate(SpawnStatus::TimedOut),
            }
        } else {
            tokio::select! {
                result = &mut wait => Outcome::Exited(result),
                _ = entry.kill_signal.notified() => Outcome::Terminate(SpawnStatus::Killed),
            }
        }
    };
    let (status, exit_code) = match outcome {
        Outcome::Exited(result) => exit_status(result),
        Outcome::Terminate(status) => {
            signal_entry_process_tree(&entry);
            let _ = child.kill().await;
            let _ = child.wait().await;
            (status, -1)
        }
    };

    // Wait for the drain tasks so captured output is complete.
    let drain_output = async {
        if let Some(task) = stdout_task {
            let _ = task.await;
        }
        if let Some(task) = stderr_task {
            let _ = task.await;
        }
    };
    tokio::pin!(drain_output);
    let (status, exit_code) = if status == SpawnStatus::Exited {
        if let Some(sleep) = timeout_sleep.as_mut() {
            tokio::select! {
                _ = &mut drain_output => (status, exit_code),
                _ = sleep.as_mut() => {
                    signal_entry_process_tree(&entry);
                    drain_output.await;
                    (SpawnStatus::TimedOut, -1)
                }
            }
        } else {
            drain_output.await;
            (status, exit_code)
        }
    } else {
        drain_output.await;
        (status, exit_code)
    };
    entry.unregister_cleanup();

    let stdout = std::mem::take(&mut *stdout_buf.lock().expect("stdout buf poisoned"));
    let stderr = std::mem::take(&mut *stderr_buf.lock().expect("stderr buf poisoned"));

    {
        let mut state = entry.state.lock().expect("spawn state poisoned");
        state.stdout = stdout;
        state.stderr = stderr;
    }
    // Record the terminal status (first-writer-wins): a concurrent `kill()` may
    // have already published `killed`, in which case we keep it and only attach
    // the drained output captured above.
    entry.record_terminal(status, exit_code);
    entry.completion.notify_waiters();
}

fn exit_status(result: std::io::Result<std::process::ExitStatus>) -> (SpawnStatus, i32) {
    match result {
        Ok(status) => (SpawnStatus::Exited, status.code().unwrap_or(-1)),
        Err(_) => (SpawnStatus::Exited, -1),
    }
}

async fn drain_into<R: AsyncReadExt + Unpin>(mut reader: R, buf: Arc<Mutex<Vec<u8>>>) {
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.lock()
                    .expect("drain buf poisoned")
                    .extend_from_slice(&chunk[..n]);
            }
        }
    }
}

fn command_display(params: &crate::value::DictMap) -> String {
    if let Some(command) = optional_string(params, "command") {
        return command;
    }
    if let Some(VmValue::List(argv)) = params.get("argv") {
        return argv
            .iter()
            .map(|v| v.display())
            .collect::<Vec<_>>()
            .join(" ");
    }
    String::new()
}

fn poll(params: &crate::value::DictMap) -> Result<VmValue, VmError> {
    let handle_id = require_param(params, "handle_id")?;
    let entry = lookup(&handle_id)?;
    let state = entry.state.lock().expect("spawn state poisoned");
    let status = state.status.unwrap_or(SpawnStatus::Running);
    let running = status == SpawnStatus::Running;

    let mut result = BTreeMap::new();
    result.put_str("handle_id", entry.handle_id.clone());
    result.put_str("status", status.as_str());
    result.insert("running".to_string(), VmValue::Bool(running));
    result.put_str("command", entry.command_display.clone());
    result.put_str("started_at", entry.started_at.clone());
    result.insert(
        "pid".to_string(),
        entry
            .pid
            .map(|p| VmValue::Int(p as i64))
            .unwrap_or(VmValue::Nil),
    );
    result.insert(
        "exit_code".to_string(),
        state
            .exit_code
            .map(|c| VmValue::Int(c as i64))
            .unwrap_or(VmValue::Nil),
    );
    result.put_str("stdout", String::from_utf8_lossy(&state.stdout));
    result.put_str("stderr", String::from_utf8_lossy(&state.stderr));
    Ok(VmValue::dict(result))
}

async fn wait(params: &crate::value::DictMap) -> Result<VmValue, VmError> {
    let handle_id = require_param(params, "handle_id")?;
    let entry = lookup(&handle_id)?;
    let timeout_ms = optional_i64(params, "timeout")
        .or_else(|| optional_i64(params, "timeout_ms"))
        .filter(|value| *value > 0)
        .map(|value| value as u64);

    // Register for the completion notification BEFORE checking status to
    // avoid a lost-wakeup race between the snapshot and the await.
    let notified = entry.completion.notified();
    tokio::pin!(notified);

    if !entry.current_status().is_terminal() {
        match timeout_ms {
            Some(ms) => {
                if tokio::time::timeout(Duration::from_millis(ms), &mut notified)
                    .await
                    .is_err()
                    && !entry.current_status().is_terminal()
                {
                    // Wait timed out and the process is still running. Leave
                    // it running per the contract; report timed_out + running.
                    let mut result = BTreeMap::new();
                    result.insert("timed_out".to_string(), VmValue::Bool(true));
                    result.insert("running".to_string(), VmValue::Bool(true));
                    result.put_str("status", "running");
                    return Ok(VmValue::dict(result));
                }
            }
            None => {
                notified.await;
            }
        }
    }

    let state = entry.state.lock().expect("spawn state poisoned");
    let status = state.status.unwrap_or(SpawnStatus::Running);
    let mut result = BTreeMap::new();
    result.put_str("status", status.as_str());
    result.insert(
        "exit_code".to_string(),
        state
            .exit_code
            .map(|c| VmValue::Int(c as i64))
            .unwrap_or(VmValue::Nil),
    );
    result.put_str("stdout", String::from_utf8_lossy(&state.stdout));
    result.put_str("stderr", String::from_utf8_lossy(&state.stderr));
    result.insert(
        "timed_out".to_string(),
        VmValue::Bool(status == SpawnStatus::TimedOut),
    );
    result.insert("running".to_string(), VmValue::Bool(false));
    Ok(VmValue::dict(result))
}

async fn kill(params: &crate::value::DictMap) -> Result<VmValue, VmError> {
    let handle_id = require_param(params, "handle_id")?;
    let entry = lookup(&handle_id)?;

    if entry.current_status().is_terminal() {
        // Already done — nothing to kill. Report current status, success.
        return Ok(kill_result(true, entry.current_status()));
    }

    // Register for the completion notification BEFORE signalling so a fast
    // reaper can't notify between here and the await below (lost-wakeup guard).
    let drained = entry.completion.notified();
    tokio::pin!(drained);
    // Ask the detached reaper to perform the real OS kill, child reap, and
    // output drain.
    signal_entry_process_tree(&entry);
    entry.kill_signal.notify_one();
    // Publish the terminal status synchronously: the kill is observably done on
    // return, regardless of when the reaper's runtime is next scheduled. Under
    // heavy parallel load the reaper (a `current_thread` test runtime, or a
    // saturated worker) can be starved well past any bounded wall-clock wait,
    // so gating the result on *observing* its transition raced the timeout and
    // spuriously reported `success: false` (the prior flake). `kill_on_drop`
    // guarantees the OS process is terminated once the reaper finishes or is
    // cancelled, and first-writer-wins keeps the reaper from clobbering this.
    let status = entry.record_terminal(SpawnStatus::Killed, -1);
    // Best-effort: briefly let the reaper finish draining captured output so a
    // follow-up wait()/poll() observes it. The success above is authoritative,
    // so a slow/starved reaper can no longer make kill report failure.
    let _ = tokio::time::timeout(Duration::from_secs(5), &mut drained).await;
    Ok(kill_result(true, status))
}

fn kill_result(success: bool, status: SpawnStatus) -> VmValue {
    let mut result = BTreeMap::new();
    result.insert("success".to_string(), VmValue::Bool(success));
    result.put_str("status", status.as_str());
    VmValue::dict(result)
}

fn signal_entry_process_tree(entry: &SpawnEntry) {
    if let Some(pid) = entry.pid {
        let mut report = crate::op_interrupt::signal_pid_tree_group_and_token_with_report(
            pid,
            Some(&entry.cleanup_token),
            9,
        );
        report.refresh_survivor_status();
        tracing::warn!(
            handle_id = %entry.handle_id,
            pid,
            children = report.children.len(),
            "host_call process.spawn signalled child process tree"
        );
    }
}

fn release(params: &crate::value::DictMap) -> Result<VmValue, VmError> {
    let handle_id = require_param(params, "handle_id")?;
    let removed = {
        let mut registry = SPAWN_REGISTRY.lock().expect("spawn registry poisoned");
        registry.remove(&handle_id)
    };
    if let Some(entry) = &removed {
        // If somehow still running, signal a kill so we never leak the
        // child after the caller has dropped its handle.
        if !entry.current_status().is_terminal() {
            signal_entry_process_tree(entry);
            entry.kill_signal.notify_one();
        }
    }
    let mut result = BTreeMap::new();
    result.insert("released".to_string(), VmValue::Bool(removed.is_some()));
    Ok(VmValue::dict(result))
}

#[cfg(all(test, unix))]
pub(crate) fn registry_len_for_test() -> usize {
    SPAWN_REGISTRY
        .lock()
        .expect("spawn registry poisoned")
        .len()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    fn params(pairs: &[(&str, VmValue)]) -> crate::value::DictMap {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn vstr(s: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(s))
    }

    fn argv(items: &[&str]) -> VmValue {
        VmValue::List(StdArc::new(items.iter().map(|s| vstr(s)).collect()))
    }

    fn get_str(dict: &VmValue, key: &str) -> String {
        match dict.as_dict().and_then(|d| d.get(key)) {
            Some(VmValue::String(s)) => s.to_string(),
            other => panic!("expected string for {key}, got {other:?}"),
        }
    }

    fn get_bool(dict: &VmValue, key: &str) -> bool {
        match dict.as_dict().and_then(|d| d.get(key)) {
            Some(VmValue::Bool(b)) => *b,
            other => panic!("expected bool for {key}, got {other:?}"),
        }
    }

    fn get_int(dict: &VmValue, key: &str) -> i64 {
        match dict.as_dict().and_then(|d| d.get(key)) {
            Some(VmValue::Int(i)) => *i,
            other => panic!("expected int for {key}, got {other:?}"),
        }
    }

    async fn spawn_argv(items: &[&str]) -> VmValue {
        spawn_argv_with_owner(items, None).await
    }

    async fn spawn_argv_with_owner(
        items: &[&str],
        owner_cancel_token: Option<StdArc<AtomicBool>>,
    ) -> VmValue {
        let p = params(&[("mode", vstr("argv")), ("argv", argv(items))]);
        dispatch("spawn", &p, owner_cancel_token)
            .await
            .expect("spawn dispatched")
            .expect("spawn ok")
    }

    #[tokio::test]
    async fn spawn_poll_wait_captures_stdout_and_exit_zero() {
        let handle = spawn_argv(&["sh", "-c", "printf hello"]).await;
        let handle_id = get_str(&handle, "handle_id");
        assert!(handle_id.starts_with("psh-"));
        assert_eq!(get_str(&handle, "status"), "running");

        let waited = dispatch("wait", &params(&[("handle_id", vstr(&handle_id))]), None)
            .await
            .expect("wait dispatched")
            .expect("wait ok");
        assert_eq!(get_str(&waited, "status"), "exited");
        assert_eq!(get_int(&waited, "exit_code"), 0);
        assert_eq!(get_str(&waited, "stdout"), "hello");
        assert!(!get_bool(&waited, "timed_out"));
        assert!(!get_bool(&waited, "running"));

        // Poll after completion is non-blocking and reports exited.
        let polled = dispatch("poll", &params(&[("handle_id", vstr(&handle_id))]), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(get_str(&polled, "status"), "exited");
        assert!(!get_bool(&polled, "running"));
        assert_eq!(get_str(&polled, "stdout"), "hello");

        dispatch("release", &params(&[("handle_id", vstr(&handle_id))]), None)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn poll_shows_running_then_exited() {
        let handle = spawn_argv(&["sh", "-c", "sleep 0.4; printf done"]).await;
        let handle_id = get_str(&handle, "handle_id");

        let polled = dispatch("poll", &params(&[("handle_id", vstr(&handle_id))]), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(get_str(&polled, "status"), "running");
        assert!(get_bool(&polled, "running"));

        let waited = dispatch("wait", &params(&[("handle_id", vstr(&handle_id))]), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(get_str(&waited, "status"), "exited");
        assert_eq!(get_str(&waited, "stdout"), "done");

        dispatch("release", &params(&[("handle_id", vstr(&handle_id))]), None)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn kill_terminates_running_process() {
        let handle = spawn_argv(&["sh", "-c", "sleep 30"]).await;
        let handle_id = get_str(&handle, "handle_id");

        let killed = dispatch("kill", &params(&[("handle_id", vstr(&handle_id))]), None)
            .await
            .unwrap()
            .unwrap();
        assert!(get_bool(&killed, "success"));
        assert_eq!(get_str(&killed, "status"), "killed");

        let polled = dispatch("poll", &params(&[("handle_id", vstr(&handle_id))]), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(get_str(&polled, "status"), "killed");
        assert!(!get_bool(&polled, "running"));

        dispatch("release", &params(&[("handle_id", vstr(&handle_id))]), None)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn timeout_ms_auto_kills() {
        let p = params(&[
            ("mode", vstr("argv")),
            ("argv", argv(&["sh", "-c", "sleep 30"])),
            ("timeout_ms", VmValue::Int(150)),
        ]);
        let handle = dispatch("spawn", &p, None).await.unwrap().unwrap();
        let handle_id = get_str(&handle, "handle_id");

        let waited = dispatch("wait", &params(&[("handle_id", vstr(&handle_id))]), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(get_str(&waited, "status"), "timed_out");
        assert!(get_bool(&waited, "timed_out"));

        dispatch("release", &params(&[("handle_id", vstr(&handle_id))]), None)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn wait_timeout_leaves_process_running() {
        let handle = spawn_argv(&["sh", "-c", "sleep 1; printf later"]).await;
        let handle_id = get_str(&handle, "handle_id");

        let waited = dispatch(
            "wait",
            &params(&[
                ("handle_id", vstr(&handle_id)),
                ("timeout_ms", VmValue::Int(100)),
            ]),
            None,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(get_bool(&waited, "timed_out"));
        assert!(get_bool(&waited, "running"));

        // Process must still be alive — a second wait completes it.
        let finished = dispatch("wait", &params(&[("handle_id", vstr(&handle_id))]), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(get_str(&finished, "status"), "exited");
        assert_eq!(get_str(&finished, "stdout"), "later");

        dispatch("release", &params(&[("handle_id", vstr(&handle_id))]), None)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn unknown_handle_errors_on_every_op() {
        for op in ["poll", "wait", "kill"] {
            let result = dispatch(
                op,
                &params(&[("handle_id", vstr("psh-deadbeef-999"))]),
                None,
            )
            .await
            .unwrap();
            assert!(result.is_err(), "{op} should error on unknown handle");
        }
        // release of an unknown handle is a no-op reporting released:false.
        let released = dispatch(
            "release",
            &params(&[("handle_id", vstr("psh-deadbeef-999"))]),
            None,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!get_bool(&released, "released"));
    }

    #[tokio::test]
    async fn release_frees_entry() {
        let handle = spawn_argv(&["sh", "-c", "printf x"]).await;
        let handle_id = get_str(&handle, "handle_id");
        dispatch("wait", &params(&[("handle_id", vstr(&handle_id))]), None)
            .await
            .unwrap()
            .unwrap();

        let released = dispatch("release", &params(&[("handle_id", vstr(&handle_id))]), None)
            .await
            .unwrap()
            .unwrap();
        assert!(get_bool(&released, "released"));

        // Now the handle is unknown.
        let polled = dispatch("poll", &params(&[("handle_id", vstr(&handle_id))]), None)
            .await
            .unwrap();
        assert!(polled.is_err());
    }

    #[tokio::test]
    async fn concurrent_spawns_are_isolated() {
        let a = spawn_argv(&["sh", "-c", "printf AAA"]).await;
        let b = spawn_argv(&["sh", "-c", "printf BBB"]).await;
        let ah = get_str(&a, "handle_id");
        let bh = get_str(&b, "handle_id");
        assert_ne!(ah, bh);

        let aw = dispatch("wait", &params(&[("handle_id", vstr(&ah))]), None)
            .await
            .unwrap()
            .unwrap();
        let bw = dispatch("wait", &params(&[("handle_id", vstr(&bh))]), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(get_str(&aw, "stdout"), "AAA");
        assert_eq!(get_str(&bw, "stdout"), "BBB");

        for h in [ah, bh] {
            dispatch("release", &params(&[("handle_id", vstr(&h))]), None)
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn registry_evicts_oldest_terminal_over_cap() {
        // Spawn a quick child and wait for it to terminate, then force the
        // registry over cap: eviction must drop terminal entries and keep
        // the registry bounded at REGISTRY_CAP.
        let owner = StdArc::new(AtomicBool::new(false));
        let mut handles = Vec::new();
        for _ in 0..(REGISTRY_CAP + 8) {
            let handle =
                spawn_argv_with_owner(&["sh", "-c", "printf z"], Some(StdArc::clone(&owner))).await;
            let handle_id = get_str(&handle, "handle_id");
            dispatch("wait", &params(&[("handle_id", vstr(&handle_id))]), None)
                .await
                .unwrap()
                .unwrap();
            handles.push(handle_id);
        }
        assert!(
            registry_len_for_test() <= REGISTRY_CAP,
            "registry exceeded cap: {}",
            registry_len_for_test()
        );

        // Clean up whatever survived eviction.
        for h in handles {
            let _ = dispatch("release", &params(&[("handle_id", vstr(&h))]), None).await;
        }
    }
}

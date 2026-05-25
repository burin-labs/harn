//! Long-running tool handle machinery.
//!
//! When a caller passes `long_running: true` to `run_command`, `run_test`, or
//! `run_build_command`, the builtin spawns the child process without waiting,
//! registers it here, and returns a handle dict immediately:
//!
//! ```json
//! {
//!   "handle_id": "hto-<pid-hex>-<n>",
//!   "started_at": "...",
//!   "command_or_op_descriptor": "..."
//! }
//! ```
//!
//! A background thread waits for the child and, when it exits, pushes a
//! `tool_result` entry into the active session's `agent_inbox` via
//! `harn_vm::orchestration::agent_inbox::push(...)` so the agent-loop's
//! next turn-preflight (or post-compaction drain) picks it up.
//!
//! ### Cancellation
//!
//! `cancel_handle(handle_id)` kills the spawned process (SIGKILL) within
//! 2 seconds. The session-end hook registered on startup kills every
//! in-flight handle associated with the ending session.
//!
//! #### PID-based signaling
//!
//! The waiter thread takes ownership of the `Child` object to drain
//! stdout/stderr and call `wait()`. To keep cancellation possible even
//! after the waiter has taken the `Child`, we store the raw OS process ID
//! in the entry and kill by PID when needed. On Unix we call `kill(2)`
//! directly via an `extern "C"` declaration (no `libc` crate required).
//! A shared `cancelled` flag suppresses the feedback push when the waiter
//! sees an exit caused by cancellation.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Duration;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::process::{self as process_handle, ProcessHandle, ProcessKiller, SpawnSpec};
use crate::tools::proc::{self, CaptureConfig, CommandStatus, EnvMode};

/// Atomic counter for generating unique handle IDs within this process.
static HANDLE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Shared cancellation state between the store entry and its waiter thread.
struct CancelState {
    /// Set to `true` when `cancel_handle` / `cancel_session_handles` runs.
    /// The waiter checks this before pushing feedback.
    cancelled: AtomicBool,
}

#[derive(Default)]
struct OutputState {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Shared state for a single in-flight child process.
struct HandleEntry {
    /// The process handle. `None` after the waiter thread takes ownership.
    handle: Option<Box<dyn ProcessHandle>>,
    /// Killer that works even after the waiter took `handle`.
    killer: Arc<dyn ProcessKiller>,
    session_id: String,
    /// Shared with the waiter thread.
    cancel_state: Arc<CancelState>,
    /// Sender used by the waiter thread to signal that the post-exit
    /// feedback push is complete. `None` if the test-side hasn't asked
    /// to be notified.
    completion_tx: Option<std::sync::mpsc::SyncSender<()>>,
}

#[derive(Default)]
struct HandleStore {
    entries: BTreeMap<String, HandleEntry>,
}

static HANDLE_STORE: LazyLock<Mutex<HandleStore>> =
    LazyLock::new(|| Mutex::new(HandleStore::default()));

/// Metadata returned to the caller immediately when a long-running spawn
/// succeeds. Serialised as a response dict by the calling builtin.
pub struct LongRunningHandleInfo {
    /// Command identifier shared with foreground command responses.
    pub command_id: String,
    /// Opaque handle identifier, e.g. `"hto-<pid-hex>-<n>"`.
    pub handle_id: String,
    /// RFC 3339 timestamp of the spawn.
    pub started_at: String,
    /// Raw child process id reported by the platform.
    pub pid: u32,
    /// Child process group id when the platform exposes it.
    pub process_group_id: Option<u32>,
    /// Human-readable display form of the argv (space-joined).
    pub command_display: String,
}

pub(crate) struct LongRunningSpawnOptions {
    pub(crate) env_mode: EnvMode,
    pub(crate) capture: CaptureConfig,
    pub(crate) session_id: String,
    pub(crate) progress_interval: Option<Duration>,
    pub(crate) progress_max_inline_bytes: usize,
}

struct WaiterContext {
    command_id: String,
    handle_id: String,
    session_id: String,
    started_at: String,
    process_group_id: Option<u32>,
    command_display: String,
    progress_interval: Option<Duration>,
    progress_max_inline_bytes: usize,
}

struct ProgressThreadContext {
    command_id: String,
    handle_id: String,
    session_id: String,
    started_at: String,
    command_display: String,
    process_group_id: Option<u32>,
    output_path: PathBuf,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    output_state: Arc<Mutex<OutputState>>,
    cancel_state: Arc<CancelState>,
    done: Arc<AtomicBool>,
    started: std::time::Instant,
    interval: Duration,
    max_inline_bytes: usize,
}

impl LongRunningHandleInfo {
    /// Convert into the standard handle response dict returned to the agent.
    pub fn into_handle_response(self) -> VmValue {
        proc::running_response(
            self.command_id,
            self.handle_id,
            self.pid,
            self.process_group_id,
            self.started_at,
            self.command_display,
        )
    }
}

/// Spawn the argv as a long-running child process and return a handle.
///
/// The background waiter pushes a `tool_result` entry into the active
/// session's `agent_inbox` when the process exits so the next
/// agent-loop turn sees the result.
pub fn spawn_long_running(
    builtin: &'static str,
    program: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    env: BTreeMap<String, String>,
    session_id: String,
) -> Result<LongRunningHandleInfo, HostlibError> {
    spawn_long_running_with_options(
        builtin,
        program,
        args,
        cwd,
        env,
        LongRunningSpawnOptions {
            env_mode: EnvMode::InheritClean,
            capture: CaptureConfig::default(),
            session_id,
            progress_interval: None,
            progress_max_inline_bytes: CaptureConfig::default().max_inline_bytes,
        },
    )
}

pub(crate) fn spawn_long_running_with_options(
    builtin: &'static str,
    program: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    env: BTreeMap<String, String>,
    options: LongRunningSpawnOptions,
) -> Result<LongRunningHandleInfo, HostlibError> {
    let spec = SpawnSpec {
        builtin,
        program: program.clone(),
        args: args.clone(),
        cwd,
        env,
        env_mode: options.env_mode,
        use_stdin: false,
        configure_process_group: true,
    };
    let handle = process_handle::spawn_process(spec)
        .map_err(|e| proc::process_error_to_hostlib(builtin, e))?;

    let pid = handle.pid().unwrap_or(0);
    let process_group_id = handle.process_group_id();
    let killer = handle.killer();
    let id = HANDLE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let handle_id = format!("hto-{:x}-{id}", std::process::id());
    let command_id = proc::next_command_id();
    let started_at = proc::now_rfc3339();

    let mut all_argv = vec![program];
    all_argv.extend(args.iter().cloned());
    let command_display = all_argv.join(" ");

    let cancel_state = Arc::new(CancelState {
        cancelled: AtomicBool::new(false),
    });

    {
        let mut store = HANDLE_STORE
            .lock()
            .expect("long-running handle store poisoned");
        store.entries.insert(
            handle_id.clone(),
            HandleEntry {
                handle: Some(handle),
                killer,
                session_id: options.session_id.clone(),
                cancel_state: cancel_state.clone(),
                completion_tx: None,
            },
        );
    }

    let waiter_context = WaiterContext {
        command_id: command_id.clone(),
        handle_id: handle_id.clone(),
        session_id: options.session_id,
        started_at: started_at.clone(),
        process_group_id,
        command_display: command_display.clone(),
        progress_interval: options.progress_interval,
        progress_max_inline_bytes: options.progress_max_inline_bytes,
    };
    let waiter_thread_name = waiter_context.handle_id.clone();
    let capture = options.capture;
    std::thread::Builder::new()
        .name(format!("hto-waiter-{waiter_thread_name}"))
        .spawn(move || {
            waiter_thread(waiter_context, cancel_state, capture);
        })
        .map_err(|e| HostlibError::Backend {
            builtin,
            message: format!("failed to spawn waiter thread: {e}"),
        })?;

    Ok(LongRunningHandleInfo {
        command_id,
        handle_id,
        started_at,
        pid,
        process_group_id,
        command_display,
    })
}

/// Background thread that waits for a child process and fires feedback.
fn waiter_thread(context: WaiterContext, cancel_state: Arc<CancelState>, capture: CaptureConfig) {
    let waiter_start = std::time::Instant::now();

    // Take the handle out of the store. If the entry is already gone (i.e.
    // cancel_handle ran and removed it before us), exit without action.
    let mut handle = {
        let mut store = HANDLE_STORE
            .lock()
            .expect("long-running handle store poisoned");
        match store.entries.get_mut(&context.handle_id) {
            Some(entry) => match entry.handle.take() {
                Some(h) => h,
                None => return, // already cancelled before we ran
            },
            None => return, // entry removed (cancelled before store insert — shouldn't happen)
        }
    };

    let output_state = Arc::new(Mutex::new(OutputState::default()));
    let done = Arc::new(AtomicBool::new(false));
    let planned = proc::planned_artifact_paths(&context.command_id);
    if let Some(parent) = planned.output_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::File::create(&planned.stdout_path);
    let _ = std::fs::File::create(&planned.stderr_path);
    let combined_file = std::fs::File::create(&planned.output_path)
        .ok()
        .map(|file| Arc::new(Mutex::new(file)));

    let stdout_thread = handle.take_stdout().map(|out| {
        spawn_output_drain(
            out,
            output_state.clone(),
            planned.stdout_path.clone(),
            combined_file.clone(),
            true,
        )
    });
    let stderr_thread = handle.take_stderr().map(|err| {
        spawn_output_drain(
            err,
            output_state.clone(),
            planned.stderr_path.clone(),
            combined_file.clone(),
            false,
        )
    });

    let progress_thread = context
        .progress_interval
        .filter(|interval| !interval.is_zero())
        .map(|interval| {
            spawn_progress_thread(ProgressThreadContext {
                command_id: context.command_id.clone(),
                handle_id: context.handle_id.clone(),
                session_id: context.session_id.clone(),
                started_at: context.started_at.clone(),
                command_display: context.command_display.clone(),
                process_group_id: context.process_group_id,
                output_path: planned.output_path.clone(),
                stdout_path: planned.stdout_path.clone(),
                stderr_path: planned.stderr_path.clone(),
                output_state: output_state.clone(),
                cancel_state: cancel_state.clone(),
                done: done.clone(),
                started: waiter_start,
                interval,
                max_inline_bytes: context.progress_max_inline_bytes,
            })
        });

    let status = handle.wait().ok();

    if let Some(thread) = stdout_thread {
        let _ = thread.join();
    }
    if let Some(thread) = stderr_thread {
        let _ = thread.join();
    }
    done.store(true, Ordering::Release);
    drop(progress_thread);
    let (stdout, stderr) = {
        let state = output_state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        (state.stdout.clone(), state.stderr.clone())
    };

    // Remove our entry from the store, taking the completion notifier on
    // the way out so we can signal it after the feedback push completes.
    let completion_tx = {
        let mut store = HANDLE_STORE
            .lock()
            .expect("long-running handle store poisoned");
        store
            .entries
            .remove(&context.handle_id)
            .and_then(|mut e| e.completion_tx.take())
    };

    let signal_done = move || {
        if let Some(tx) = completion_tx {
            let _ = tx.try_send(());
        }
    };

    // If cancellation was requested, don't push feedback — the caller
    // that cancelled doesn't want to receive a spurious tool_result.
    if cancel_state.cancelled.load(Ordering::Acquire) {
        signal_done();
        return;
    }

    let (exit_code, signal_name) = match status {
        Some(s) => decode_exit_status(s),
        // wait() itself failed — treat as killed (extremely unusual).
        None => (-1, Some("SIGKILL".to_string())),
    };
    let duration = waiter_start.elapsed();
    let duration_ms = duration.as_millis() as i64;
    let artifacts = match proc::persist_artifacts(
        &context.command_id,
        &stdout,
        &stderr,
        Some(&context.handle_id),
    ) {
        Ok(artifacts) => artifacts,
        Err(_) => return,
    };
    let (inline_stdout, inline_stderr) = proc::inline_output(&stdout, &stderr, capture);

    let mut payload = serde_json::Map::new();
    payload.insert(
        "command_id".into(),
        serde_json::Value::String(context.command_id.clone()),
    );
    payload.insert(
        "status".into(),
        serde_json::Value::String(CommandStatus::Completed.as_str().to_string()),
    );
    payload.insert(
        "handle_id".into(),
        serde_json::Value::String(context.handle_id),
    );
    payload.insert(
        "command_or_op_descriptor".into(),
        serde_json::Value::String(context.command_display),
    );
    payload.insert(
        "started_at".into(),
        serde_json::Value::String(context.started_at),
    );
    payload.insert(
        "ended_at".into(),
        serde_json::Value::String(proc::now_rfc3339()),
    );
    payload.insert(
        "duration_ms".into(),
        serde_json::Value::Number(duration_ms.into()),
    );
    payload.insert(
        "exit_code".into(),
        serde_json::Value::Number(exit_code.into()),
    );
    payload.insert("stdout".into(), serde_json::Value::String(inline_stdout));
    payload.insert("stderr".into(), serde_json::Value::String(inline_stderr));
    payload.insert(
        "output_path".into(),
        serde_json::Value::String(artifacts.output_path.display().to_string()),
    );
    payload.insert(
        "stdout_path".into(),
        serde_json::Value::String(artifacts.stdout_path.display().to_string()),
    );
    payload.insert(
        "stderr_path".into(),
        serde_json::Value::String(artifacts.stderr_path.display().to_string()),
    );
    payload.insert(
        "line_count".into(),
        serde_json::Value::Number(artifacts.line_count.into()),
    );
    payload.insert(
        "byte_count".into(),
        serde_json::Value::Number(artifacts.byte_count.into()),
    );
    payload.insert(
        "output_sha256".into(),
        serde_json::Value::String(artifacts.output_sha256),
    );
    if let Some(pgid) = context.process_group_id {
        payload.insert(
            "process_group_id".into(),
            serde_json::Value::Number((pgid as u64).into()),
        );
    }
    if let Some(sig) = signal_name {
        payload.insert("signal".into(), serde_json::Value::String(sig));
    } else {
        payload.insert("signal".into(), serde_json::Value::Null);
    }

    let content = serde_json::to_string(&payload).unwrap_or_default();
    harn_vm::orchestration::agent_inbox::push(
        &context.session_id,
        "tool_result",
        &content,
        "hostlib.long_running.exit",
    );
    signal_done();
}

fn spawn_output_drain(
    mut reader: Box<dyn Read + Send>,
    state: Arc<Mutex<OutputState>>,
    path: std::path::PathBuf,
    combined_file: Option<Arc<Mutex<std::fs::File>>>,
    stdout: bool,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut file = std::fs::File::create(path).ok();
        let mut buf = [0_u8; 8192];
        loop {
            let read = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => break,
            };
            let chunk = &buf[..read];
            if let Some(file) = file.as_mut() {
                let _ = file.write_all(chunk);
            }
            if let Some(combined) = combined_file.as_ref() {
                if let Ok(mut combined) = combined.lock() {
                    let _ = combined.write_all(chunk);
                }
            }
            if let Ok(mut state) = state.lock() {
                if stdout {
                    state.stdout.extend_from_slice(chunk);
                } else {
                    state.stderr.extend_from_slice(chunk);
                }
            }
        }
    })
}

fn spawn_progress_thread(context: ProgressThreadContext) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while !context.done.load(Ordering::Acquire)
            && !context.cancel_state.cancelled.load(Ordering::Acquire)
        {
            std::thread::sleep(context.interval);
            if context.done.load(Ordering::Acquire)
                || context.cancel_state.cancelled.load(Ordering::Acquire)
            {
                break;
            }
            let (stdout, stderr) = {
                let state = context
                    .output_state
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                (state.stdout.clone(), state.stderr.clone())
            };
            let capture = CaptureConfig {
                max_inline_bytes: context.max_inline_bytes,
                ..CaptureConfig::default()
            };
            let (inline_stdout, inline_stderr) = proc::inline_output(&stdout, &stderr, capture);
            let byte_count = stdout.len().saturating_add(stderr.len());
            let payload = serde_json::json!({
                "command_id": &context.command_id,
                "handle_id": &context.handle_id,
                "status": CommandStatus::Running.as_str(),
                "command_or_op_descriptor": &context.command_display,
                "started_at": &context.started_at,
                "ended_at": null,
                "duration_ms": context.started.elapsed().as_millis() as i64,
                "exit_code": null,
                "signal": null,
                "stdout": inline_stdout,
                "stderr": inline_stderr,
                "output_path": context.output_path.display().to_string(),
                "stdout_path": context.stdout_path.display().to_string(),
                "stderr_path": context.stderr_path.display().to_string(),
                "byte_count": byte_count as i64,
                "line_count": stdout.iter().chain(stderr.iter()).filter(|byte| **byte == b'\n').count() as i64,
                "process_group_id": context.process_group_id,
            });
            harn_vm::orchestration::agent_inbox::push(
                &context.session_id,
                "tool_progress",
                &payload.to_string(),
                "hostlib.long_running.progress",
            );
        }
    })
}

/// Cancel a specific in-flight long-running handle. Kills the process and
/// removes the entry. Returns `true` if the handle was found and cancelled.
pub fn cancel_handle(handle_id: &str) -> bool {
    let (handle_owned, killer, cancel_state, completion_tx) = {
        let mut store = HANDLE_STORE
            .lock()
            .expect("long-running handle store poisoned");
        match store.entries.remove(handle_id) {
            None => return false,
            Some(mut entry) => (
                entry.handle.take(),
                entry.killer.clone(),
                entry.cancel_state.clone(),
                entry.completion_tx.take(),
            ),
        }
    };
    do_kill(handle_owned, killer, cancel_state);
    // If a test registered a completion notifier, signal it now — the
    // waiter won't be able to (entry already removed) and we know the
    // waiter will skip feedback because cancellation is set.
    if let Some(tx) = completion_tx {
        let _ = tx.try_send(());
    }
    true
}

/// Tuple shape used by `cancel_session_handles` to drain entries while
/// holding the store lock for as little as possible. Boxed-trait fields
/// make it noisy to inline as an unnamed type.
type SessionKillEntry = (
    Option<Box<dyn ProcessHandle>>,
    Arc<dyn ProcessKiller>,
    Arc<CancelState>,
);

/// Cancel all in-flight handles for a given session. Called by the
/// session-end hook to avoid orphaned processes.
pub fn cancel_session_handles(session_id: &str) {
    let to_kill: Vec<SessionKillEntry> = {
        let mut store = HANDLE_STORE
            .lock()
            .expect("long-running handle store poisoned");
        let matching: Vec<String> = store
            .entries
            .iter()
            .filter(|(_, e)| e.session_id == session_id)
            .map(|(id, _)| id.clone())
            .collect();
        matching
            .into_iter()
            .filter_map(|id| {
                store.entries.remove(&id).map(|mut e| {
                    let handle = e.handle.take();
                    (handle, e.killer.clone(), e.cancel_state.clone())
                })
            })
            .collect()
    };
    for (handle, killer, cancel_state) in to_kill {
        do_kill(handle, killer, cancel_state);
    }
}

/// Set the cancellation flag and kill the process. Used by both `cancel_handle`
/// and `cancel_session_handles`.
fn do_kill(
    handle: Option<Box<dyn ProcessHandle>>,
    killer: Arc<dyn ProcessKiller>,
    cancel_state: Arc<CancelState>,
) {
    // Signal cancellation so the waiter (if still running) skips feedback.
    cancel_state.cancelled.store(true, Ordering::Release);
    // Kill via the handle's killer (works whether or not we still own
    // the handle). If we still hold the handle, drop it after kill so the
    // OS reaps the child.
    killer.kill();
    drop(handle);
}

/// Register the session-cleanup hook with harn-vm. Uses a `OnceLock` so the
/// hook is registered exactly once even if `register_builtins` is called
/// multiple times (e.g. in tests).
pub(crate) fn register_cleanup_hook() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        let hook: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|session_id: &str| {
            cancel_session_handles(session_id);
        });
        harn_vm::register_session_end_hook(hook);
    });
}

fn decode_exit_status(status: process_handle::ExitStatus) -> (i32, Option<String>) {
    if let Some(code) = status.code {
        return (code, None);
    }
    if let Some(sig) = status.signal {
        return (-1, Some(format!("SIG{sig}")));
    }
    (-1, None)
}

/// Register a completion notifier for `handle_id`. The waiter thread sends
/// `()` on the returned receiver after it pushes the feedback item to the
/// global queue. Returns `None` if the handle is no longer in the store
/// (e.g. already cancelled or completed). Used by tests to await waiter
/// completion deterministically — no polling, no `thread::sleep`.
pub fn register_completion_notifier(handle_id: &str) -> Option<std::sync::mpsc::Receiver<()>> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<()>(1);
    let mut store = HANDLE_STORE
        .lock()
        .expect("long-running handle store poisoned");
    let entry = store.entries.get_mut(handle_id)?;
    entry.completion_tx = Some(tx);
    Some(rx)
}

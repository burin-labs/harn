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
//! sees an exit caused by cancellation. Callers that need artifact-stable
//! cancellation can opt into waiting for the waiter result through
//! `cancel_handle`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use harn_vm::VmDictExt;
use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::json::vm_dict_to_json;
use crate::process::{self as process_handle, ProcessHandle, ProcessKiller, SpawnSpec};
use crate::tools::args::to_agent_path;
use crate::tools::proc::{self, CaptureConfig, CommandStatus, EnvMode};

/// Atomic counter for generating unique handle IDs within this process.
static HANDLE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Shared cancellation state between the store entry and its waiter thread.
///
/// The waiter must never observe a terminal cancellation before its cleanup
/// receipt is available. A single mutex makes that publication atomic even
/// though `ProcessKiller::kill` wakes the process waiter before it returns its
/// cleanup report.
#[derive(Default)]
struct CancelState {
    state: Mutex<CancellationState>,
}

#[derive(Default)]
struct CancellationState {
    /// A canceller owns the terminal result publication while this is set.
    cancellation_requested: bool,
    /// A cancellation result has been published for the waiter.
    cancelled: bool,
    /// Whether the published cancellation represents a timeout.
    timed_out: bool,
    /// Structural process-tree cleanup evidence returned by the killer.
    process_cleanup: Option<process_handle::ProcessCleanupReport>,
    /// The waiter has committed a terminal result, so later cancellation is a
    /// no-op rather than retroactively rewriting a completed outcome.
    completed: bool,
}

#[derive(Clone, Debug)]
struct CancellationSnapshot {
    cancelled: bool,
    timed_out: bool,
    process_cleanup: Option<process_handle::ProcessCleanupReport>,
}

impl CancelState {
    fn begin_cancellation(&self, timed_out: bool) -> Option<MutexGuard<'_, CancellationState>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.cancellation_requested || state.completed {
            return None;
        }
        state.cancellation_requested = true;
        state.timed_out = timed_out;
        Some(state)
    }

    fn complete_wait(&self) -> CancellationSnapshot {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.completed = true;
        CancellationSnapshot {
            cancelled: state.cancelled,
            timed_out: state.timed_out,
            process_cleanup: state.process_cleanup.clone(),
        }
    }

    fn cancellation_published(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .cancelled
    }
}

impl CancellationState {
    fn record_cleanup(&mut self, report: process_handle::ProcessCleanupReport) {
        match self.process_cleanup.as_mut() {
            Some(existing) => existing.merge(report),
            None => self.process_cleanup = Some(report),
        }
    }

    fn publish_cancellation(&mut self) {
        debug_assert!(self.cancellation_requested);
        self.cancelled = true;
    }
}

fn kill_and_publish(killer: &dyn ProcessKiller, cancellation: &mut CancellationState) {
    let report = killer.kill();
    cancellation.record_cleanup(report);
    cancellation.publish_cancellation();
}

#[derive(Default)]
pub(crate) struct OutputState {
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) combined: Vec<u8>,
    pub(crate) terminal: Option<VmValue>,
    /// Wall-clock instant of the most recent stdout/stderr chunk, used to derive
    /// `silence_ms` on progress snapshots (the byte-stall decision trigger).
    /// `None` until the first byte of output arrives.
    last_output_at: Option<std::time::Instant>,
}

pub(crate) struct OutputFeed {
    pub(crate) state: Mutex<OutputState>,
    notify: tokio::sync::Notify,
}

impl Default for OutputFeed {
    fn default() -> Self {
        Self {
            state: Mutex::new(OutputState::default()),
            notify: tokio::sync::Notify::new(),
        }
    }
}

impl OutputFeed {
    pub(crate) fn notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.notify.notified()
    }
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
    output_feed: Arc<OutputFeed>,
    /// Sender used by the waiter thread to signal that the post-exit
    /// feedback push is complete. `None` if the test-side hasn't asked
    /// to be notified.
    completion_tx: Option<std::sync::mpsc::SyncSender<()>>,
    /// One-shot result channels installed by callers that need to synchronize
    /// on the finalized command result instead of observing it indirectly
    /// through the session inbox.
    result_txs: Vec<std::sync::mpsc::SyncSender<VmValue>>,
    /// Opaque verification snapshot binding provided by the caller.
    snapshot_binding: Option<harn_vm::value::DictMap>,
    /// Spawn-time lease tag surfaced by `list_handles` (loop owns transitions).
    lease: LeaseTag,
    /// Human-readable command display, so `list_handles` can render a ledger
    /// digest without the caller re-deriving it.
    command_display: String,
    /// RFC 3339 spawn timestamp, for `list_handles` elapsed reporting.
    started_at: String,
}

#[derive(Default)]
struct HandleStore {
    entries: BTreeMap<String, HandleEntry>,
}

static HANDLE_STORE: LazyLock<Mutex<HandleStore>> =
    LazyLock::new(|| Mutex::new(HandleStore::default()));

type HandleNotifiers = (
    Option<std::sync::mpsc::SyncSender<()>>,
    Vec<std::sync::mpsc::SyncSender<VmValue>>,
);

fn take_handle_notifiers(handle_id: &str) -> HandleNotifiers {
    let mut store = HANDLE_STORE
        .lock()
        .expect("long-running handle store poisoned");
    store
        .entries
        .remove(handle_id)
        .map(|mut entry| {
            (
                entry.completion_tx.take(),
                std::mem::take(&mut entry.result_txs),
            )
        })
        .unwrap_or((None, Vec::new()))
}

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
    /// Opaque verification snapshot binding provided by the caller.
    pub snapshot_binding: Option<harn_vm::value::DictMap>,
}

/// Default ceiling for the progress-emission backoff schedule when the caller
/// does not pin `progress_max_interval_ms`. The schedule starts at the base
/// `progress_interval`, doubles after each snapshot, and is clamped here — so a
/// multi-minute command emits a handful of re-entries (2s, 4s, 8s, 16s, 30s,
/// 30s, ...) instead of one every base interval, keeping a silent long build
/// token-cheap while still surfacing early, frequent progress.
const DEFAULT_PROGRESS_MAX_INTERVAL: Duration = Duration::from_secs(30);

pub(crate) struct LongRunningSpawnOptions {
    pub(crate) env_mode: EnvMode,
    pub(crate) env_remove: Vec<String>,
    pub(crate) capture: CaptureConfig,
    pub(crate) session_id: String,
    pub(crate) progress_interval: Option<Duration>,
    pub(crate) progress_max_interval: Option<Duration>,
    pub(crate) progress_max_inline_bytes: usize,
    pub(crate) snapshot_binding: Option<harn_vm::value::DictMap>,
    /// Initial lease classification recorded on the handle entry for
    /// `list_handles` reporting. `"awaited"` (the loop schedules decision
    /// re-entries and waits on it) or `"service"` (detached; runs until the
    /// session-end reaper). The loop owns transitions after spawn; this is only
    /// the spawn-time tag.
    pub(crate) lease: LeaseTag,
}

/// Spawn-time lease classification stored on a handle entry. The agent loop's
/// ledger owns lease transitions (e.g. `release_command` awaited -> service);
/// this tag is the initial value surfaced by `list_handles`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LeaseTag {
    Awaited,
    Service,
}

impl LeaseTag {
    fn as_str(self) -> &'static str {
        match self {
            LeaseTag::Awaited => "awaited",
            LeaseTag::Service => "service",
        }
    }
}

struct WaiterContext {
    command_id: String,
    handle_id: String,
    session_id: String,
    started_at: String,
    process_group_id: Option<u32>,
    command_display: String,
    progress_interval: Option<Duration>,
    progress_max_interval: Option<Duration>,
    progress_max_inline_bytes: usize,
    snapshot_binding: Option<harn_vm::value::DictMap>,
    output_feed: Arc<OutputFeed>,
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
    output_feed: Arc<OutputFeed>,
    cancel_state: Arc<CancelState>,
    done: Arc<AtomicBool>,
    started: std::time::Instant,
    /// Base delay before the first progress snapshot and the seed of the
    /// doubling backoff schedule.
    interval: Duration,
    /// Upper bound the doubling schedule is clamped to.
    max_interval: Duration,
    max_inline_bytes: usize,
    snapshot_binding: Option<harn_vm::value::DictMap>,
}

impl LongRunningHandleInfo {
    /// Convert into the standard handle response dict returned to the agent.
    pub fn into_handle_response(self) -> VmValue {
        let Self {
            command_id,
            handle_id,
            started_at,
            pid,
            process_group_id,
            command_display,
            snapshot_binding,
        } = self;
        proc::running_response(
            command_id,
            handle_id,
            pid,
            process_group_id,
            started_at,
            command_display,
            snapshot_binding.as_ref(),
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
            env_remove: Vec::new(),
            capture: CaptureConfig::default(),
            session_id,
            progress_interval: None,
            progress_max_interval: None,
            progress_max_inline_bytes: CaptureConfig::default().max_inline_bytes,
            snapshot_binding: None,
            lease: LeaseTag::Awaited,
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
    let mut env = env;
    proc::apply_toolchain_path(cwd.as_deref(), &mut env, options.env_mode);
    let spec = SpawnSpec {
        builtin,
        program: program.clone(),
        args: args.clone(),
        cwd,
        env,
        env_remove: options.env_remove.clone(),
        env_mode: options.env_mode,
        use_stdin: false,
        configure_process_group: true,
        output_capture: process_handle::OutputCapture::Pipe,
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
    let _artifacts = proc::register_live_artifacts(&command_id, Some(&handle_id))?;

    let mut all_argv = vec![program];
    all_argv.extend(args.iter().cloned());
    let command_display = all_argv.join(" ");

    let cancel_state = Arc::new(CancelState {
        state: Mutex::new(CancellationState::default()),
    });
    let output_feed = Arc::new(OutputFeed::default());

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
                output_feed: output_feed.clone(),
                completion_tx: None,
                result_txs: Vec::new(),
                snapshot_binding: options.snapshot_binding.clone(),
                lease: options.lease,
                command_display: command_display.clone(),
                started_at: started_at.clone(),
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
        progress_max_interval: options.progress_max_interval,
        progress_max_inline_bytes: options.progress_max_inline_bytes,
        snapshot_binding: options.snapshot_binding.clone(),
        output_feed,
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
        snapshot_binding: options.snapshot_binding,
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
            context.output_feed.clone(),
            planned.stdout_path.clone(),
            combined_file.clone(),
            true,
        )
    });
    let stderr_thread = handle.take_stderr().map(|err| {
        spawn_output_drain(
            err,
            context.output_feed.clone(),
            planned.stderr_path.clone(),
            combined_file.clone(),
            false,
        )
    });

    let progress_thread = context
        .progress_interval
        .filter(|interval| !interval.is_zero())
        .map(|interval| {
            // The backoff ceiling is never below the base interval: a caller that
            // pins a large base without a cap gets a fixed cadence, not a cap that
            // silently shrinks its interval.
            let max_interval = context
                .progress_max_interval
                .filter(|cap| !cap.is_zero())
                .unwrap_or(DEFAULT_PROGRESS_MAX_INTERVAL)
                .max(interval);
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
                output_feed: context.output_feed.clone(),
                cancel_state: cancel_state.clone(),
                done: done.clone(),
                started: waiter_start,
                interval,
                max_interval,
                max_inline_bytes: context.progress_max_inline_bytes,
                snapshot_binding: context.snapshot_binding.clone(),
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
        let state = context
            .output_feed
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        (state.stdout.clone(), state.stderr.clone())
    };

    let cancellation = cancel_state.complete_wait();
    let cancelled = cancellation.cancelled;
    let timed_out = cancelled && cancellation.timed_out;
    let process_cleanup = cancellation.process_cleanup;

    let (exit_code, signal_name) = match status {
        Some(s) => decode_exit_status(s),
        // wait() itself failed — treat as killed (extremely unusual).
        None => (-1, Some("SIGKILL".to_string())),
    };
    let command_status = if timed_out {
        CommandStatus::TimedOut
    } else if cancelled {
        CommandStatus::Killed
    } else {
        CommandStatus::Completed
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
        Err(error) => {
            tracing::warn!(
                "long-running command {} could not persist artifacts: {error}; returning in-memory terminal metadata",
                context.command_id
            );
            proc::summarize_artifacts(
                &context.command_id,
                &stdout,
                &stderr,
                Some(&context.handle_id),
            )
        }
    };
    let (inline_stdout, inline_stderr) = proc::inline_output(&stdout, &stderr, capture);

    let mut payload = serde_json::Map::new();
    payload.insert(
        "command_id".into(),
        serde_json::Value::String(context.command_id.clone()),
    );
    payload.insert(
        "status".into(),
        serde_json::Value::String(command_status.as_str().to_string()),
    );
    payload.insert(
        "handle_id".into(),
        serde_json::Value::String(context.handle_id.clone()),
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
    payload.insert("timed_out".into(), serde_json::Value::Bool(timed_out));
    payload.insert("stdout".into(), serde_json::Value::String(inline_stdout));
    payload.insert("stderr".into(), serde_json::Value::String(inline_stderr));
    payload.insert(
        "output_path".into(),
        serde_json::Value::String(to_agent_path(&artifacts.output_path)),
    );
    payload.insert(
        "stdout_path".into(),
        serde_json::Value::String(to_agent_path(&artifacts.stdout_path)),
    );
    payload.insert(
        "stderr_path".into(),
        serde_json::Value::String(to_agent_path(&artifacts.stderr_path)),
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
    if let Some(snapshot_binding) = context.snapshot_binding.as_ref() {
        payload.insert("snapshot_binding".into(), vm_dict_to_json(snapshot_binding));
    }
    if let Some(process_cleanup) = process_cleanup.as_ref() {
        payload.insert(
            "process_cleanup".into(),
            proc::process_cleanup_to_json(process_cleanup),
        );
    }

    let result_value = harn_vm::json_to_vm_value(&serde_json::Value::Object(payload.clone()));
    {
        let mut state = context
            .output_feed
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.terminal = Some(result_value.clone());
    }
    context.output_feed.notify.notify_waiters();
    if !cancelled {
        let content = serde_json::to_string(&payload).unwrap_or_default();
        harn_vm::orchestration::agent_inbox::push(
            &context.session_id,
            "tool_result",
            &content,
            "hostlib.long_running.exit",
        );
    }
    // Remove our entry from the store only after the public feedback path is
    // published. An explicit `wait_command` can register a direct waiter while
    // the child has exited but artifacts are still being finalized; waking that
    // waiter after the inbox push lets `wait_command` consume the matching
    // inbox entry and preserve the old no-duplicate-feedback contract.
    let (completion_tx, result_txs) = take_handle_notifiers(&context.handle_id);
    for tx in result_txs {
        let _ = tx.try_send(result_value.clone());
    }
    if let Some(tx) = completion_tx {
        let _ = tx.try_send(());
    }
}

fn spawn_output_drain(
    mut reader: Box<dyn Read + Send>,
    output_feed: Arc<OutputFeed>,
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
            if let Ok(mut state) = output_feed.state.lock() {
                if let Some(combined) = combined_file.as_ref() {
                    if let Ok(mut combined) = combined.lock() {
                        let _ = combined.write_all(chunk);
                    }
                }
                if stdout {
                    state.stdout.extend_from_slice(chunk);
                } else {
                    state.stderr.extend_from_slice(chunk);
                }
                state.combined.extend_from_slice(chunk);
                state.last_output_at = Some(std::time::Instant::now());
            }
            output_feed.notify.notify_waiters();
        }
    })
}

/// Next delay in the progress backoff schedule: double the current delay,
/// clamped to `max`. Saturates to `max` rather than overflowing.
fn next_progress_interval(current: Duration, max: Duration) -> Duration {
    current.checked_mul(2).unwrap_or(max).min(max)
}

fn spawn_progress_thread(context: ProgressThreadContext) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        // Exponential backoff: wait `interval`, emit a snapshot, then double the
        // wait after each snapshot up to `max_interval`. Progress is frequent
        // while the model most wants to know whether the command is moving, and
        // thins out for a long-running command so it stays token-cheap. The final
        // completion snapshot is emitted by the waiter thread on exit, independent
        // of where this schedule is, so the terminal result is never delayed by a
        // long backoff wait.
        let mut current = context.interval;
        while !context.done.load(Ordering::Acquire)
            && !context.cancel_state.cancellation_published()
        {
            std::thread::sleep(current);
            if context.done.load(Ordering::Acquire) || context.cancel_state.cancellation_published()
            {
                break;
            }
            current = next_progress_interval(current, context.max_interval);
            let (stdout, stderr, last_output_at) = {
                let state = context
                    .output_feed
                    .state
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                (
                    state.stdout.clone(),
                    state.stderr.clone(),
                    state.last_output_at,
                )
            };
            let capture = CaptureConfig {
                max_inline_bytes: context.max_inline_bytes,
                ..CaptureConfig::default()
            };
            let (inline_stdout, inline_stderr) = proc::inline_output(&stdout, &stderr, capture);
            let byte_count = stdout.len().saturating_add(stderr.len());
            // Milliseconds since the last output chunk (or since spawn if the
            // command has produced nothing yet). The loop reads this to detect a
            // byte-stall and to escalate a silent hang toward the ceiling.
            let silence_ms = last_output_at
                .map(|instant| instant.elapsed().as_millis() as i64)
                .unwrap_or_else(|| context.started.elapsed().as_millis() as i64);
            let mut payload = serde_json::json!({
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
                "output_path": to_agent_path(&context.output_path),
                "stdout_path": to_agent_path(&context.stdout_path),
                "stderr_path": to_agent_path(&context.stderr_path),
                "byte_count": byte_count as i64,
                // Monotonic combined-output offset the loop passes to
                // `read_command_output` to page only the delta since its last
                // digest (never re-paying for the cumulative tail).
                "output_offset": byte_count as i64,
                // Loop derives the "first stderr after a clean run" decision
                // trigger from this count crossing zero; kept loop-side so all
                // event-edge detection lives in one place (spec §1.4).
                "stderr_byte_count": stderr.len() as i64,
                "silence_ms": silence_ms,
                "line_count": stdout.iter().chain(stderr.iter()).filter(|byte| **byte == b'\n').count() as i64,
                "process_group_id": context.process_group_id,
            });
            if let (Some(object), Some(snapshot_binding)) =
                (payload.as_object_mut(), context.snapshot_binding.as_ref())
            {
                object.insert(
                    "snapshot_binding".to_string(),
                    vm_dict_to_json(snapshot_binding),
                );
            }
            harn_vm::orchestration::agent_inbox::push(
                &context.session_id,
                "tool_progress",
                &payload.to_string(),
                "hostlib.long_running.progress",
            );
        }
    })
}

pub(crate) struct CancelOptions {
    pub(crate) timed_out: bool,
    pub(crate) wait_result: Option<Duration>,
}

pub(crate) struct CancelOutcome {
    pub(crate) cancelled: bool,
    pub(crate) result: Option<VmValue>,
}

/// Cancel a specific in-flight long-running handle. Kills the process and lets
/// the waiter drain output/artifacts. Returns `true` if the handle was found
/// and cancellation was newly requested.
pub fn cancel_handle(handle_id: &str) -> bool {
    cancel_handle_with_options(
        handle_id,
        CancelOptions {
            timed_out: false,
            wait_result: None,
        },
    )
    .cancelled
}

pub(crate) fn snapshot_binding_for_handle(handle_id: &str) -> Option<harn_vm::value::DictMap> {
    let store = HANDLE_STORE
        .lock()
        .expect("long-running handle store poisoned");
    store
        .entries
        .get(handle_id)
        .and_then(|entry| entry.snapshot_binding.clone())
}

pub(crate) fn output_feed_for_handle(handle_id: &str) -> Option<Arc<OutputFeed>> {
    HANDLE_STORE
        .lock()
        .expect("long-running handle store poisoned")
        .entries
        .get(handle_id)
        .map(|entry| entry.output_feed.clone())
}

pub(crate) fn cancel_handle_with_options(handle_id: &str, options: CancelOptions) -> CancelOutcome {
    let (killer, cancel_state, result_rx) = {
        let mut store = HANDLE_STORE
            .lock()
            .expect("long-running handle store poisoned");
        let Some((killer, cancel_state)) = store
            .entries
            .get(handle_id)
            .map(|entry| (entry.killer.clone(), entry.cancel_state.clone()))
        else {
            return CancelOutcome {
                cancelled: false,
                result: None,
            };
        };
        let result_rx = options.wait_result.map(|_| {
            let (tx, rx) = std::sync::mpsc::sync_channel::<VmValue>(1);
            store
                .entries
                .get_mut(handle_id)
                .expect("handle entry disappeared while store was locked")
                .result_txs
                .push(tx);
            rx
        });
        (killer, cancel_state, result_rx)
    };
    let cancellation = cancel_state.begin_cancellation(options.timed_out);
    let Some(mut cancellation) = cancellation else {
        return CancelOutcome {
            cancelled: false,
            result: match (options.wait_result, result_rx) {
                (Some(timeout), Some(rx)) => rx.recv_timeout(timeout).ok(),
                _ => None,
            },
        };
    };
    kill_and_publish(killer.as_ref(), &mut cancellation);
    drop(cancellation);
    let result = match (options.wait_result, result_rx) {
        (Some(timeout), Some(rx)) => rx.recv_timeout(timeout).ok(),
        _ => None,
    };
    CancelOutcome {
        cancelled: true,
        result,
    }
}

/// Wait for a live long-running handle to finalize and return its result.
///
/// Returns `None` when the handle is already gone or the timeout elapses. The
/// result is also published through the session inbox for normal agent-loop
/// delivery; callers that use this direct synchronizer should drain the
/// matching inbox item after receiving the value if they are consuming it.
pub(crate) fn wait_for_result(handle_id: &str, timeout: Duration) -> Option<VmValue> {
    if timeout.is_zero() {
        return None;
    }
    let rx = {
        let mut store = HANDLE_STORE
            .lock()
            .expect("long-running handle store poisoned");
        let entry = store.entries.get_mut(handle_id)?;
        let (tx, rx) = std::sync::mpsc::sync_channel::<VmValue>(1);
        entry.result_txs.push(tx);
        rx
    };
    rx.recv_timeout(timeout).ok()
}

/// Live handles for `session_id`, for the agent loop's ledger reconciliation and
/// digest rendering. Each row carries the spawn-time lease tag, command display,
/// and start timestamp; scheduling and lease transitions live in the loop. An
/// entry disappears from this list the instant its waiter thread removes it on
/// process exit, so a completed-and-drained command is never reported as live.
pub(crate) fn list_session_handles(session_id: &str) -> VmValue {
    let store = HANDLE_STORE
        .lock()
        .expect("long-running handle store poisoned");
    let handles: Vec<VmValue> = store
        .entries
        .iter()
        .filter(|(_id, entry)| entry.session_id == session_id)
        .map(|(id, entry)| {
            let mut row = harn_vm::value::DictMap::new();
            row.put_str("handle_id", id.clone());
            row.put_str("session_id", entry.session_id.clone());
            row.put_str("lease", entry.lease.as_str());
            row.put_str("command_or_op_descriptor", entry.command_display.clone());
            row.put_str("started_at", entry.started_at.clone());
            VmValue::dict(row)
        })
        .collect();
    let mut response = harn_vm::value::DictMap::new();
    response.insert(
        harn_vm::value::intern_key("handles"),
        VmValue::List(Arc::new(handles)),
    );
    VmValue::dict(response)
}

/// Tuple shape used by `cancel_session_handles` to drain entries while
/// holding the store lock for as little as possible. Boxed-trait fields
/// make it noisy to inline as an unnamed type.
type SessionKillEntry = (Arc<dyn ProcessKiller>, Arc<CancelState>);

/// Cancel all in-flight handles for a given session. Called by the
/// session-end hook to avoid orphaned processes.
pub fn cancel_session_handles(session_id: &str) {
    let to_kill: Vec<SessionKillEntry> = {
        let store = HANDLE_STORE
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
                let entry = store.entries.get(&id)?;
                Some((entry.killer.clone(), entry.cancel_state.clone()))
            })
            .collect()
    };
    for (killer, cancel_state) in to_kill {
        if let Some(mut cancellation) = cancel_state.begin_cancellation(false) {
            kill_and_publish(killer.as_ref(), &mut cancellation);
        }
    }
}

/// Register the session-cleanup hook with harn-vm. Uses a `OnceLock` so the
/// hook is registered exactly once even if `register_builtins` is called
/// multiple times (e.g. in tests).
pub(crate) fn register_cleanup_hook() {
    static REGISTERED: OnceLock<harn_vm::SessionEndHookRegistration> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        let hook: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|session_id: &str| {
            cancel_session_handles(session_id);
        });
        harn_vm::register_session_end_hook(hook)
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

/// Register a result notifier for `handle_id`.
///
/// This is a narrow test/diagnostic hook for the same synchronization path
/// `wait_command` uses. Normal callers should use the `wait_command` tool so
/// the matching session-inbox feedback is consumed consistently.
pub fn register_result_notifier(handle_id: &str) -> Option<std::sync::mpsc::Receiver<VmValue>> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<VmValue>(1);
    let mut store = HANDLE_STORE
        .lock()
        .expect("long-running handle store poisoned");
    let entry = store.entries.get_mut(handle_id)?;
    entry.result_txs.push(tx);
    Some(rx)
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    use super::{next_progress_interval, CancelState};
    use crate::process::ProcessCleanupReport;

    #[test]
    fn progress_backoff_doubles_then_clamps_to_max() {
        let max = Duration::from_secs(30);
        // Doubles from the base while below the cap.
        assert_eq!(
            next_progress_interval(Duration::from_secs(2), max),
            Duration::from_secs(4)
        );
        assert_eq!(
            next_progress_interval(Duration::from_secs(8), max),
            Duration::from_secs(16)
        );
        // Clamps to the cap once doubling would exceed it, and stays there.
        assert_eq!(next_progress_interval(Duration::from_secs(16), max), max);
        assert_eq!(next_progress_interval(max, max), max);
        // Saturates instead of overflowing at the Duration ceiling.
        assert_eq!(next_progress_interval(Duration::MAX, max), max);
    }

    #[test]
    fn terminal_snapshot_waits_for_cleanup_publication() {
        let state = Arc::new(CancelState::default());
        let mut publication = state
            .begin_cancellation(true)
            .expect("fresh handle should accept cancellation");
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (snapshot_tx, snapshot_rx) = mpsc::sync_channel(1);
        let waiter_state = state.clone();
        let waiter = std::thread::spawn(move || {
            started_tx.send(()).expect("test waiter start receiver");
            snapshot_tx
                .send(waiter_state.complete_wait())
                .expect("test snapshot receiver");
        });

        started_rx.recv().expect("test waiter did not start");
        assert!(matches!(
            snapshot_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        publication.record_cleanup(ProcessCleanupReport::for_signal(Some(42), 9));
        publication.publish_cancellation();
        drop(publication);

        let snapshot = snapshot_rx.recv().expect("waiter did not publish snapshot");
        waiter.join().expect("test waiter panicked");
        assert!(snapshot.cancelled);
        assert!(snapshot.timed_out);
        assert_eq!(
            snapshot
                .process_cleanup
                .expect("cleanup must publish with cancellation")
                .root_pid,
            Some(42)
        );
        assert!(
            state.begin_cancellation(false).is_none(),
            "a completed terminal result must not be retroactively cancelled"
        );
    }
}

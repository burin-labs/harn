//! Long-running tool handle machinery.
//!
//! When a caller passes `long_running: true` to `run_command`, `run_test`, or
//! `run_build_command`, the builtin spawns the child process without waiting,
//! registers it here, and returns a handle dict immediately:
//!
//! ```json
//! { "handle_id": "hto-<pid-hex>-<n>", "started_at": <unix_ms>, "command": "..." }
//! ```
//!
//! A background thread waits for the child and, when it exits, calls
//! `harn_vm::push_pending_feedback_global(session_id, "tool_result", json)`
//! so the agent-loop's next turn-preflight picks it up.
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
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use harn_vm::VmValue;

use harn_vm::process_sandbox;

use crate::error::HostlibError;

/// Atomic counter for generating unique handle IDs within this process.
static HANDLE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Shared cancellation state between the store entry and its waiter thread.
struct CancelState {
    /// Set to `true` when `cancel_handle` / `cancel_session_handles` runs.
    /// The waiter checks this before pushing feedback.
    cancelled: AtomicBool,
}

/// Shared state for a single in-flight child process.
struct HandleEntry {
    /// The child process. `None` after the waiter thread takes ownership.
    child: Option<Child>,
    /// Raw OS process ID — available even after the waiter took `child`.
    pid: u32,
    session_id: String,
    /// Shared with the waiter thread.
    cancel_state: Arc<CancelState>,
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
    /// Opaque handle identifier, e.g. `"hto-<pid-hex>-<n>"`.
    pub handle_id: String,
    /// Unix timestamp of the spawn, in milliseconds.
    pub started_at_ms: u64,
    /// Human-readable display form of the argv (space-joined).
    pub command_display: String,
}

impl LongRunningHandleInfo {
    /// Convert into the standard handle response dict returned to the agent.
    pub fn into_handle_response(self) -> VmValue {
        super::response::ResponseBuilder::new()
            .str("handle_id", self.handle_id)
            .int("started_at", self.started_at_ms as i64)
            .str("command", self.command_display)
            .build()
    }
}

/// Spawn the argv as a long-running child process and return a handle.
///
/// The background waiter calls `push_pending_feedback_global` when the
/// process exits so the next agent-loop turn sees the result.
pub fn spawn_long_running(
    builtin: &'static str,
    program: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    env: BTreeMap<String, String>,
    session_id: String,
) -> Result<LongRunningHandleInfo, HostlibError> {
    if program.is_empty() {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param: "argv",
            message: "first element of argv must be a non-empty program name".to_string(),
        });
    }

    let mut command =
        process_sandbox::std_command_for(&program, &args).map_err(|e| HostlibError::Backend {
            builtin,
            message: format!("sandbox setup failed: {e:?}"),
        })?;

    if let Some(cwd_path) = cwd.as_ref() {
        process_sandbox::enforce_process_cwd(cwd_path).map_err(|e| HostlibError::Backend {
            builtin,
            message: format!("sandbox cwd rejected: {e:?}"),
        })?;
        command.current_dir(cwd_path);
    }

    if !env.is_empty() {
        command.env_clear();
        for (key, value) in &env {
            command.env(key, value);
        }
    }

    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.stdin(Stdio::null());

    let child = command.spawn().map_err(|e| {
        if let Some(violation) = process_sandbox::process_spawn_error(&e) {
            return HostlibError::Backend {
                builtin,
                message: format!("sandbox rejected spawn: {violation:?}"),
            };
        }
        HostlibError::Backend {
            builtin,
            message: format!("spawn failed: {e}"),
        }
    })?;

    let pid = child.id();
    let id = HANDLE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let handle_id = format!("hto-{:x}-{id}", std::process::id());

    let started_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let mut all_argv = vec![program.clone()];
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
                child: Some(child),
                pid,
                session_id: session_id.clone(),
                cancel_state: cancel_state.clone(),
            },
        );
    }

    let waiter_handle_id = handle_id.clone();
    let waiter_session_id = session_id;
    std::thread::Builder::new()
        .name(format!("hto-waiter-{waiter_handle_id}"))
        .spawn(move || {
            waiter_thread(waiter_handle_id, waiter_session_id, cancel_state);
        })
        .map_err(|e| HostlibError::Backend {
            builtin,
            message: format!("failed to spawn waiter thread: {e}"),
        })?;

    Ok(LongRunningHandleInfo {
        handle_id,
        started_at_ms,
        command_display,
    })
}

/// Background thread that waits for a child process and fires feedback.
fn waiter_thread(handle_id: String, session_id: String, cancel_state: Arc<CancelState>) {
    let waiter_start = std::time::Instant::now();

    // Take the child out of the store. If the entry is already gone (i.e.
    // cancel_handle ran and removed it before us), exit without action.
    let mut child = {
        let mut store = HANDLE_STORE
            .lock()
            .expect("long-running handle store poisoned");
        match store.entries.get_mut(&handle_id) {
            Some(entry) => match entry.child.take() {
                Some(c) => c,
                None => return, // already cancelled before we ran
            },
            None => return, // entry removed (cancelled before store insert — shouldn't happen)
        }
    };

    // Drain stdout/stderr on separate threads to prevent pipe deadlock.
    use std::io::Read;
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let (out_tx, out_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let (err_tx, err_rx) = std::sync::mpsc::channel::<Vec<u8>>();

    if let Some(mut out) = child.stdout.take() {
        std::thread::spawn(move || {
            let _ = out.read_to_end(&mut stdout_bytes);
            let _ = out_tx.send(stdout_bytes);
        });
    }
    if let Some(mut err) = child.stderr.take() {
        std::thread::spawn(move || {
            let _ = err.read_to_end(&mut stderr_bytes);
            let _ = err_tx.send(stderr_bytes);
        });
    }

    let status = child.wait().ok();

    let stdout = out_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    let stderr = err_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();

    // Remove our entry from the store.
    {
        let mut store = HANDLE_STORE
            .lock()
            .expect("long-running handle store poisoned");
        store.entries.remove(&handle_id);
    }

    // If cancellation was requested, don't push feedback — the caller
    // that cancelled doesn't want to receive a spurious tool_result.
    if cancel_state.cancelled.load(Ordering::Acquire) {
        return;
    }

    let (exit_code, signal_name) = match status {
        Some(s) => decode_exit_status(s),
        // wait() itself failed — treat as killed (extremely unusual).
        None => (-1, Some("SIGKILL".to_string())),
    };
    let duration_ms = waiter_start.elapsed().as_millis() as i64;

    let mut payload = serde_json::Map::new();
    payload.insert("handle_id".into(), serde_json::Value::String(handle_id));
    payload.insert(
        "exit_code".into(),
        serde_json::Value::Number(exit_code.into()),
    );
    payload.insert(
        "stdout".into(),
        serde_json::Value::String(String::from_utf8_lossy(&stdout).into_owned()),
    );
    payload.insert(
        "stderr".into(),
        serde_json::Value::String(String::from_utf8_lossy(&stderr).into_owned()),
    );
    payload.insert(
        "duration_ms".into(),
        serde_json::Value::Number(duration_ms.into()),
    );
    if let Some(sig) = signal_name {
        payload.insert("signal".into(), serde_json::Value::String(sig));
    } else {
        payload.insert("signal".into(), serde_json::Value::Null);
    }

    let content = serde_json::to_string(&payload).unwrap_or_default();
    harn_vm::push_pending_feedback_global(&session_id, "tool_result", &content);
}

/// Cancel a specific in-flight long-running handle. Kills the process and
/// removes the entry. Returns `true` if the handle was found and cancelled.
pub fn cancel_handle(handle_id: &str) -> bool {
    let (pid, child, cancel_state) = {
        let mut store = HANDLE_STORE
            .lock()
            .expect("long-running handle store poisoned");
        match store.entries.remove(handle_id) {
            None => return false,
            Some(mut entry) => (entry.pid, entry.child.take(), entry.cancel_state.clone()),
        }
    };
    do_kill(pid, child, cancel_state);
    true
}

/// Cancel all in-flight handles for a given session. Called by the
/// session-end hook to avoid orphaned processes.
pub fn cancel_session_handles(session_id: &str) {
    let to_kill: Vec<(u32, Option<Child>, Arc<CancelState>)> = {
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
                    let child = e.child.take();
                    (e.pid, child, e.cancel_state.clone())
                })
            })
            .collect()
    };
    for (pid, child, cancel_state) in to_kill {
        do_kill(pid, child, cancel_state);
    }
}

/// Set the cancellation flag and kill the process. Used by both `cancel_handle`
/// and `cancel_session_handles`.
fn do_kill(pid: u32, child: Option<Child>, cancel_state: Arc<CancelState>) {
    // Signal cancellation so the waiter (if still running) skips feedback.
    cancel_state.cancelled.store(true, Ordering::Release);
    if let Some(mut c) = child {
        // Waiter hasn't taken the child yet — kill it directly.
        kill_child(&mut c);
    } else {
        // Waiter already took the child; signal by PID.
        kill_pid(pid);
    }
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

fn kill_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Kill a process by its PID. Used when the waiter thread has already taken
/// ownership of the `Child` object but the process must still be terminated.
fn kill_pid(pid: u32) {
    #[cfg(unix)]
    {
        // SAFETY: We call kill(2) with a valid PID and SIGKILL (9). On all
        // Unix targets pid_t and int are i32. No libc crate needed.
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        unsafe {
            kill(pid as i32, 9); // SIGKILL
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid; // No-op on non-Unix; TerminateProcess would require winapi.
    }
}

fn decode_exit_status(status: std::process::ExitStatus) -> (i32, Option<String>) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(code) = status.code() {
            return (code, None);
        }
        if let Some(sig) = status.signal() {
            return (-1, Some(format!("SIG{sig}")));
        }
        (-1, None)
    }
    #[cfg(not(unix))]
    (status.code().unwrap_or(-1), None)
}

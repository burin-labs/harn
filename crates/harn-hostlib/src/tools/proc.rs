//! Shared subprocess spawn / wait / timeout machinery used by every
//! `tools/run_*` and `tools/manage_packages` builtin.
//!
//! All process tools funnel through here so:
//! 1. Each spawn goes through the [`crate::process::ProcessSpawner`]
//!    currently installed (default: real spawner backed by
//!    `harn_vm::process_sandbox`), so the active orchestration capability
//!    policy applies (Linux seccomp/landlock, macOS `sandbox-exec`,
//!    workspace-root cwd enforcement). Tests install a mock spawner so
//!    every test in `tests/process_tools.rs` is deterministic and
//!    free of wall-clock dependence.
//! 2. Pipe drains run on background threads so >64 KB output never
//!    deadlocks `wait()`.
//! 3. Timeout enforcement is uniform: when a deadline elapses, the child
//!    is killed and `timed_out: true` is reported in the response.

use harn_vm::VmDictExt;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use harn_vm::VmValue;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::error::HostlibError;
use crate::process::{
    self as process_handle, OutputCapture, ProcessError, ProcessKiller, SpawnSpec,
};
use crate::tools::args::to_agent_path;
use crate::tools::response::ResponseBuilder;

mod artifacts;
mod toolchain_path;

pub(crate) use self::artifacts::{
    live_artifact_snapshot, live_artifact_tail, persist_artifacts, planned_artifact_paths,
    register_live_artifacts, resolve_output_path, summarize_artifacts,
};

static COMMAND_COUNTER: AtomicU64 = AtomicU64::new(1);

const DEFAULT_MAX_INLINE_BYTES: usize = 50_000;

/// Resolved request payload for a subprocess spawn.
#[derive(Debug, Clone)]
pub(crate) struct SpawnRequest {
    pub(crate) builtin: &'static str,
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) env_remove: Vec<String>,
    pub(crate) env_mode: EnvMode,
    pub(crate) stdin: Option<String>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) capture: CaptureConfig,
}

/// Result of running a subprocess to completion (or to the deadline).
#[derive(Debug, Clone)]
pub(crate) struct SpawnOutcome {
    pub(crate) command_id: String,
    pub(crate) status: CommandStatus,
    pub(crate) pid: Option<u32>,
    pub(crate) process_group_id: Option<u32>,
    pub(crate) started_at: String,
    pub(crate) ended_at: Option<String>,
    pub(crate) exit_code: i32,
    pub(crate) signal: Option<String>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) output_path: PathBuf,
    pub(crate) stdout_path: PathBuf,
    pub(crate) stderr_path: PathBuf,
    pub(crate) line_count: u64,
    pub(crate) byte_count: u64,
    pub(crate) output_sha256: String,
    pub(crate) duration: Duration,
    pub(crate) timed_out: bool,
    pub(crate) process_cleanup: Option<process_handle::ProcessCleanupReport>,
}

pub(crate) use crate::process::EnvMode;

#[derive(Debug, Clone, Copy)]
pub(crate) struct CaptureConfig {
    pub(crate) stdout: bool,
    pub(crate) stderr: bool,
    pub(crate) merge_stderr: bool,
    pub(crate) max_inline_bytes: usize,
    pub(crate) transport: CaptureTransport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureTransport {
    Pipe,
    File,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            stdout: true,
            stderr: true,
            merge_stderr: false,
            max_inline_bytes: DEFAULT_MAX_INLINE_BYTES,
            transport: CaptureTransport::Pipe,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandStatus {
    Completed,
    Running,
    TimedOut,
    Killed,
}

impl CommandStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            CommandStatus::Completed => "completed",
            CommandStatus::Running => "running",
            CommandStatus::TimedOut => "timed_out",
            CommandStatus::Killed => "killed",
        }
    }
}

/// Spawn the configured command, capture stdout + stderr in full, and
/// enforce the timeout. Translates spawn / sandbox failures into
/// `HostlibError::Backend` so the surrounding builtin gets a uniform
/// `Thrown` dict on the script side.
pub(crate) fn run(req: SpawnRequest) -> Result<SpawnOutcome, HostlibError> {
    let started = std::time::Instant::now();
    let started_at = now_rfc3339();
    let command_id = next_command_id();

    let mut env = req.env.clone();
    apply_toolchain_path(req.cwd.as_deref(), &mut env, req.env_mode);
    let file_capture = match req.capture.transport {
        CaptureTransport::Pipe => None,
        CaptureTransport::File => Some(FileCapture::new(req.builtin)?),
    };

    let spec = SpawnSpec {
        builtin: req.builtin,
        program: req.program.clone(),
        args: req.args.clone(),
        cwd: req.cwd.clone(),
        env,
        env_remove: req.env_remove.clone(),
        env_mode: req.env_mode,
        use_stdin: req.stdin.is_some(),
        // Foreground children get their own process group too, so the
        // interrupt path below (scope cancellation / deadline expiry) can
        // reap grandchildren with a single group signal.
        configure_process_group: true,
        output_capture: file_capture
            .as_ref()
            .map(FileCapture::output_capture)
            .unwrap_or(OutputCapture::Pipe),
    };
    let mut handle = process_handle::spawn_process(spec)
        .map_err(|e| process_error_to_hostlib(req.builtin, e))?;

    let pid = handle.pid();
    let process_group_id = handle.process_group_id();
    let killer = handle.killer();

    if let Some(stdin_data) = req.stdin.as_ref() {
        if let Some(mut stdin) = handle.take_stdin() {
            use std::io::Write;
            // A failed write is non-fatal — the child may have closed stdin
            // immediately. We surface the eventual exit code regardless.
            let _ = stdin.write_all(stdin_data.as_bytes());
        }
    }

    let (out_rx, stdout_thread) = spawn_reader_thread(handle.take_stdout());
    let (err_rx, stderr_thread) = spawn_reader_thread(handle.take_stderr());

    // Poll `harn_vm::op_interrupt::requested` alongside the child wait so
    // scope cancellation, `deadline` expiry, and VM drop terminate the
    // child's process group instead of orphaning it. `background: true`
    // spawns bypass this path (see `tools/long_running.rs`) and remain the
    // fire-and-forget escape hatch.
    let deadline = req.timeout.map(|timeout| started + timeout);
    let wait_result = handle.wait_with_timeout(req.timeout, &harn_vm::op_interrupt::requested);
    if wait_result.is_err() {
        let _ = killer.kill();
    }

    let outcome = wait_result.map_err(|error| HostlibError::Backend {
        builtin: req.builtin,
        message: format!("wait failed: {error}"),
    })?;
    let wait_killed = matches!(
        outcome,
        process_handle::WaitOutcome::TimedOut(_) | process_handle::WaitOutcome::Interrupted(_)
    );
    let (stdout_drain, stderr_drain) = if file_capture.is_some() {
        (DrainResult::empty(), DrainResult::empty())
    } else {
        let stdout_drain = drain_output(out_rx, deadline, wait_killed, &killer);
        let stderr_drain = drain_output(
            err_rx,
            deadline,
            wait_killed || stdout_drain.process_cleanup.is_some(),
            &killer,
        );
        (stdout_drain, stderr_drain)
    };
    // Do not join unconditionally: a descendant that inherited stdout/stderr can
    // keep the pipe open after the direct child exits. The drain helper enforces
    // the same command deadline/interrupt and kills the cleanup-token family.
    drop(stdout_thread);
    drop(stderr_thread);

    let (stdout_bytes, stderr_bytes) = match file_capture.as_ref() {
        Some(files) => files.read(req.builtin)?,
        None => (stdout_drain.bytes, stderr_drain.bytes),
    };
    let drain_timed_out = stdout_drain.timed_out || stderr_drain.timed_out;
    let drain_interrupted = stdout_drain.interrupted || stderr_drain.interrupted;
    let mut drain_cleanup = stdout_drain.process_cleanup;
    if let Some(stderr_cleanup) = stderr_drain.process_cleanup {
        if let Some(existing) = drain_cleanup.as_mut() {
            existing.merge(stderr_cleanup);
        } else {
            drain_cleanup = Some(stderr_cleanup);
        }
    }

    let ended_at = Some(now_rfc3339());

    let (mut command_status, mut exit_code, mut signal, mut timed_out, mut process_cleanup) =
        match outcome {
            process_handle::WaitOutcome::Exited(status) => {
                let (exit_code, signal) = decode_status(status);
                (CommandStatus::Completed, exit_code, signal, false, None)
            }
            process_handle::WaitOutcome::TimedOut(report) => (
                CommandStatus::TimedOut,
                -1,
                Some("SIGKILL".to_string()),
                true,
                Some(report),
            ),
            process_handle::WaitOutcome::Interrupted(report) => (
                CommandStatus::Killed,
                -1,
                Some("SIGTERM".to_string()),
                false,
                Some(report),
            ),
        };
    if drain_timed_out || drain_interrupted {
        if let Some(cleanup) = drain_cleanup {
            if let Some(existing) = process_cleanup.as_mut() {
                existing.merge(cleanup);
            } else {
                process_cleanup = Some(cleanup);
            }
        }
        if drain_timed_out {
            command_status = CommandStatus::TimedOut;
            exit_code = -1;
            signal = Some("SIGKILL".to_string());
            timed_out = true;
        } else {
            command_status = CommandStatus::Killed;
            exit_code = -1;
            signal = Some("SIGTERM".to_string());
        }
    }
    let artifacts = persist_artifacts(&command_id, &stdout_bytes, &stderr_bytes, None)?;
    let (stdout, stderr) = inline_output(&stdout_bytes, &stderr_bytes, req.capture);

    Ok(SpawnOutcome {
        command_id,
        status: command_status,
        pid,
        process_group_id,
        started_at,
        ended_at,
        exit_code,
        signal,
        stdout,
        stderr,
        output_path: artifacts.output_path,
        stdout_path: artifacts.stdout_path,
        stderr_path: artifacts.stderr_path,
        line_count: artifacts.line_count,
        byte_count: artifacts.byte_count,
        output_sha256: artifacts.output_sha256,
        duration: started.elapsed(),
        timed_out,
        process_cleanup,
    })
}

#[derive(Debug)]
struct DrainResult {
    bytes: Vec<u8>,
    timed_out: bool,
    interrupted: bool,
    process_cleanup: Option<process_handle::ProcessCleanupReport>,
}

impl DrainResult {
    fn empty() -> Self {
        Self {
            bytes: Vec::new(),
            timed_out: false,
            interrupted: false,
            process_cleanup: None,
        }
    }
}

struct FileCapture {
    stdout: tempfile::NamedTempFile,
    stderr: tempfile::NamedTempFile,
}

impl FileCapture {
    fn new(builtin: &'static str) -> Result<Self, HostlibError> {
        let stdout = tempfile::Builder::new()
            .prefix("harn-command-stdout-")
            .tempfile()
            .map_err(|error| HostlibError::Backend {
                builtin,
                message: format!("create stdout capture: {error}"),
            })?;
        let stderr = tempfile::Builder::new()
            .prefix("harn-command-stderr-")
            .tempfile()
            .map_err(|error| HostlibError::Backend {
                builtin,
                message: format!("create stderr capture: {error}"),
            })?;
        Ok(Self { stdout, stderr })
    }

    fn output_capture(&self) -> OutputCapture {
        OutputCapture::File {
            stdout_path: self.stdout.path().to_path_buf(),
            stderr_path: self.stderr.path().to_path_buf(),
        }
    }

    fn read(&self, builtin: &'static str) -> Result<(Vec<u8>, Vec<u8>), HostlibError> {
        let stdout = std::fs::read(self.stdout.path()).map_err(|error| HostlibError::Backend {
            builtin,
            message: format!("read stdout capture: {error}"),
        })?;
        let stderr = std::fs::read(self.stderr.path()).map_err(|error| HostlibError::Backend {
            builtin,
            message: format!("read stderr capture: {error}"),
        })?;
        Ok((stdout, stderr))
    }
}

fn spawn_reader_thread(
    reader: Option<Box<dyn Read + Send>>,
) -> (mpsc::Receiver<Vec<u8>>, Option<thread::JoinHandle<()>>) {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let Some(mut reader) = reader else {
        drop(tx);
        return (rx, None);
    };
    let handle = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    (rx, Some(handle))
}

fn drain_output(
    rx: mpsc::Receiver<Vec<u8>>,
    deadline: Option<std::time::Instant>,
    already_killed: bool,
    killer: &Arc<dyn ProcessKiller>,
) -> DrainResult {
    use std::sync::mpsc::RecvTimeoutError;

    if already_killed {
        return DrainResult {
            bytes: collect_available_output(&rx, Duration::from_millis(100)),
            timed_out: false,
            interrupted: false,
            process_cleanup: None,
        };
    }

    loop {
        let interrupted = harn_vm::op_interrupt::requested();
        let now = std::time::Instant::now();
        let timed_out = deadline.is_some_and(|deadline| now >= deadline);
        if interrupted || timed_out {
            let cleanup = killer.kill();
            return DrainResult {
                bytes: collect_available_output(&rx, Duration::from_millis(100)),
                timed_out,
                interrupted,
                process_cleanup: Some(cleanup),
            };
        }
        let wait = deadline
            .map(|deadline| {
                deadline
                    .saturating_duration_since(now)
                    .min(Duration::from_millis(20))
            })
            .unwrap_or_else(|| Duration::from_millis(20));
        match rx.recv_timeout(wait) {
            Ok(bytes) => {
                let mut all_bytes = bytes;
                for next in rx.try_iter() {
                    all_bytes.extend(next);
                }
                return DrainResult {
                    bytes: all_bytes,
                    timed_out: false,
                    interrupted: false,
                    process_cleanup: None,
                };
            }
            Err(RecvTimeoutError::Disconnected) => {
                return DrainResult {
                    bytes: Vec::new(),
                    timed_out: false,
                    interrupted: false,
                    process_cleanup: None,
                };
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn collect_available_output(rx: &mpsc::Receiver<Vec<u8>>, first_wait: Duration) -> Vec<u8> {
    let mut bytes = match rx.recv_timeout(first_wait) {
        Ok(bytes) => bytes,
        Err(_) => return Vec::new(),
    };
    for next in rx.try_iter() {
        bytes.extend(next);
    }
    bytes
}

/// Apply the mise/asdf toolchain PATH normalizer to a run() child environment
/// before it is handed to the spawner. The effective working directory is the
/// command's `cwd` (when supplied) or the hostlib process cwd. Gated behind
/// `HARN_RUN_TOOLCHAIN_PATH` and declaration-gated, so this is byte-identical
/// to the previous behavior on repos without a recognized version file (or with
/// the gate off). See `tools/proc/toolchain_path.rs`.
pub(crate) fn apply_toolchain_path(
    cwd: Option<&Path>,
    env: &mut BTreeMap<String, String>,
    env_mode: EnvMode,
) {
    let effective_cwd = match cwd {
        Some(cwd) => cwd.to_path_buf(),
        None => match std::env::current_dir() {
            Ok(dir) => dir,
            // No cwd to anchor detection on — leave PATH untouched.
            Err(_) => return,
        },
    };
    toolchain_path::normalize_child_env(&effective_cwd, env, env_mode);
}

pub(crate) fn process_error_to_hostlib(builtin: &'static str, err: ProcessError) -> HostlibError {
    match err {
        ProcessError::InvalidArgv(message) => HostlibError::InvalidParameter {
            builtin,
            param: "argv",
            message,
        },
        ProcessError::SandboxSetup(message) => HostlibError::Backend {
            builtin,
            message: format!("sandbox setup failed: {message}"),
        },
        ProcessError::SandboxCwd(message) => HostlibError::Backend {
            builtin,
            message: format!("sandbox cwd rejected: {message}"),
        },
        ProcessError::SandboxSpawn(message) => HostlibError::Backend {
            builtin,
            message: format!("sandbox rejected spawn: {message}"),
        },
        ProcessError::Spawn(message) => HostlibError::Backend {
            builtin,
            message: format!("spawn failed: {message}"),
        },
        ProcessError::CatastrophicFloor(message) => {
            HostlibError::CatastrophicFloor { builtin, message }
        }
    }
}

pub(crate) fn build_response(
    outcome: SpawnOutcome,
    handle_id: Option<String>,
    policy_context: Option<harn_vm::value::DictMap>,
) -> VmValue {
    let mut builder = ResponseBuilder::new()
        .str("command_id", outcome.command_id.clone())
        .str("status", outcome.status.as_str())
        .int("duration_ms", outcome.duration.as_millis() as i64)
        .int("exit_code", outcome.exit_code as i64)
        .opt_str("signal", outcome.signal)
        .bool("timed_out", outcome.timed_out)
        .str("stdout", outcome.stdout)
        .str("stderr", outcome.stderr)
        .str("output_path", to_agent_path(&outcome.output_path))
        .str("stdout_path", to_agent_path(&outcome.stdout_path))
        .str("stderr_path", to_agent_path(&outcome.stderr_path))
        .int("line_count", outcome.line_count as i64)
        .int("byte_count", outcome.byte_count as i64)
        .str("output_sha256", outcome.output_sha256)
        .str("started_at", outcome.started_at)
        .str("audit_id", format!("audit_{}", outcome.command_id));
    builder = match outcome.ended_at {
        Some(ended_at) => builder.str("ended_at", ended_at),
        None => builder.nil("ended_at"),
    };
    builder = match outcome.pid {
        Some(pid) => builder.int("pid", pid as i64),
        None => builder.nil("pid"),
    };
    builder = match outcome.process_group_id {
        Some(pgid) => builder.int("process_group_id", pgid as i64),
        None => builder.nil("process_group_id"),
    };
    if let Some(process_cleanup) = outcome.process_cleanup.as_ref() {
        builder = builder.dict("process_cleanup", process_cleanup_to_dict(process_cleanup));
    }
    builder = match handle_id {
        Some(handle_id) => builder.str("handle_id", handle_id),
        None => builder.nil("handle_id"),
    };
    let mut sandbox = harn_vm::value::DictMap::new();
    sandbox.put_str("kind", sandbox_kind());
    sandbox.insert(
        harn_vm::value::intern_key("enforced"),
        VmValue::Bool(sandbox_enforced()),
    );
    builder = builder.dict("sandbox", sandbox);
    if let Some(policy_context) = policy_context {
        builder = builder.dict("policy_context", policy_context);
    }
    builder.build()
}

pub(crate) fn process_cleanup_to_dict(
    report: &process_handle::ProcessCleanupReport,
) -> harn_vm::value::DictMap {
    let reaped_children = report
        .children
        .iter()
        .filter(|child| child.alive_after_cleanup == Some(false))
        .map(process_cleanup_child_to_value)
        .collect::<Vec<_>>();
    let surviving_children = report
        .children
        .iter()
        .filter(|child| child.alive_after_cleanup == Some(true))
        .map(process_cleanup_child_to_value)
        .collect::<Vec<_>>();
    let observed_children = report
        .children
        .iter()
        .map(process_cleanup_child_to_value)
        .collect::<Vec<_>>();

    let mut dict = harn_vm::value::DictMap::new();
    match report.root_pid {
        Some(pid) => dict.put_int("root_pid", pid as i64),
        None => {
            dict.insert(harn_vm::value::intern_key("root_pid"), VmValue::Nil);
        }
    }
    dict.insert(
        harn_vm::value::intern_key("attempted_signals"),
        VmValue::List(Arc::new(
            report
                .attempted_signals
                .iter()
                .map(|signal| VmValue::String(arcstr::ArcStr::from(format_signal(*signal))))
                .collect(),
        )),
    );
    dict.put_int("observed_child_count", report.children.len() as i64);
    dict.put_int("reaped_child_count", reaped_children.len() as i64);
    dict.put_int("survivor_count", surviving_children.len() as i64);
    dict.insert(
        harn_vm::value::intern_key("observed_children"),
        VmValue::List(Arc::new(observed_children)),
    );
    dict.insert(
        harn_vm::value::intern_key("reaped_children"),
        VmValue::List(Arc::new(reaped_children)),
    );
    dict.insert(
        harn_vm::value::intern_key("surviving_children"),
        VmValue::List(Arc::new(surviving_children)),
    );
    dict
}

pub(crate) fn process_cleanup_to_json(
    report: &process_handle::ProcessCleanupReport,
) -> serde_json::Value {
    let dict = process_cleanup_to_dict(report);
    crate::json::vm_dict_to_json(&dict)
}

fn process_cleanup_child_to_value(child: &process_handle::ProcessCleanupChild) -> VmValue {
    let mut dict = harn_vm::value::DictMap::new();
    dict.put_int("pid", child.pid as i64);
    match child.parent_pid {
        Some(parent_pid) => dict.put_int("parent_pid", parent_pid as i64),
        None => {
            dict.insert(harn_vm::value::intern_key("parent_pid"), VmValue::Nil);
        }
    }
    dict.put_int("depth", child.depth as i64);
    match child.command_name.as_ref() {
        Some(command_name) => dict.put_str("command_name", command_name),
        None => {
            dict.insert(harn_vm::value::intern_key("command_name"), VmValue::Nil);
        }
    }
    dict.insert(
        harn_vm::value::intern_key("signals"),
        VmValue::List(Arc::new(
            child
                .signals
                .iter()
                .map(|signal| VmValue::String(arcstr::ArcStr::from(format_signal(*signal))))
                .collect(),
        )),
    );
    match child.alive_after_cleanup {
        Some(alive) => dict.insert(
            harn_vm::value::intern_key("alive_after_cleanup"),
            VmValue::Bool(alive),
        ),
        None => dict.insert(
            harn_vm::value::intern_key("alive_after_cleanup"),
            VmValue::Nil,
        ),
    };
    VmValue::dict(dict)
}

pub(crate) fn running_response(
    command_id: String,
    handle_id: String,
    pid: u32,
    process_group_id: Option<u32>,
    started_at: String,
    command_display: String,
    snapshot_binding: Option<&harn_vm::value::DictMap>,
) -> VmValue {
    let artifacts = planned_artifact_paths(&command_id);
    let mut sandbox = harn_vm::value::DictMap::new();
    sandbox.put_str("kind", sandbox_kind());
    sandbox.insert(
        harn_vm::value::intern_key("enforced"),
        VmValue::Bool(sandbox_enforced()),
    );
    let mut builder = ResponseBuilder::new()
        .str("command_id", command_id.clone())
        .str("status", CommandStatus::Running.as_str())
        .int("pid", pid as i64)
        .str("handle_id", handle_id)
        .str("started_at", started_at)
        .nil("ended_at")
        .int("duration_ms", 0)
        .nil("exit_code")
        .nil("signal")
        .bool("timed_out", false)
        .str("stdout", "")
        .str("stderr", "")
        .str("output_path", to_agent_path(&artifacts.output_path))
        .str("stdout_path", to_agent_path(&artifacts.stdout_path))
        .str("stderr_path", to_agent_path(&artifacts.stderr_path))
        .int("line_count", 0)
        .int("byte_count", 0)
        .str("output_sha256", "")
        .dict("sandbox", sandbox)
        .str("audit_id", format!("audit_{command_id}"))
        .str("command", command_display.clone())
        .str("command_or_op_descriptor", command_display);
    builder = match process_group_id {
        Some(pgid) => builder.int("process_group_id", pgid as i64),
        None => builder.nil("process_group_id"),
    };
    if let Some(snapshot_binding) = snapshot_binding {
        builder = builder.dict("snapshot_binding", snapshot_binding.clone());
    }
    builder.build()
}

pub(crate) fn next_command_id() -> String {
    let id = COMMAND_COUNTER.fetch_add(1, Ordering::SeqCst);
    let now_nanos = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("cmd_{}_{}_{}", std::process::id(), now_nanos, id)
}

pub(crate) fn now_rfc3339() -> String {
    let now: OffsetDateTime = SystemTime::now().into();
    now.format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

pub(crate) fn inline_output(
    stdout: &[u8],
    stderr: &[u8],
    capture: CaptureConfig,
) -> (String, String) {
    if capture.merge_stderr {
        let mut merged = Vec::with_capacity(stdout.len() + stderr.len() + 1);
        merged.extend_from_slice(stdout);
        if !stdout.is_empty() && !stdout.ends_with(b"\n") && !stderr.is_empty() {
            merged.push(b'\n');
        }
        merged.extend_from_slice(stderr);
        return (
            if capture.stdout {
                lossy_prefix(&merged, capture.max_inline_bytes)
            } else {
                String::new()
            },
            String::new(),
        );
    }
    (
        if capture.stdout {
            lossy_prefix(stdout, capture.max_inline_bytes)
        } else {
            String::new()
        },
        if capture.stderr {
            lossy_prefix(stderr, capture.max_inline_bytes)
        } else {
            String::new()
        },
    )
}

fn lossy_prefix(bytes: &[u8], max_inline_bytes: usize) -> String {
    let cap = bytes.len().min(max_inline_bytes);
    match std::str::from_utf8(&bytes[..cap]) {
        Ok(text) => text.to_string(),
        Err(error) if error.error_len().is_none() => {
            String::from_utf8_lossy(&bytes[..error.valid_up_to()]).into_owned()
        }
        Err(_) => String::from_utf8_lossy(&bytes[..cap]).into_owned(),
    }
}

pub(crate) fn sandbox_kind() -> &'static str {
    if cfg!(target_os = "linux") {
        "landlock"
    } else if cfg!(target_os = "macos") {
        "sandbox-exec"
    } else if cfg!(target_os = "windows") {
        "appcontainer"
    } else {
        "none"
    }
}

pub(crate) fn sandbox_enforced() -> bool {
    harn_vm::orchestration::current_execution_policy().is_some()
}

fn decode_status(status: process_handle::ExitStatus) -> (i32, Option<String>) {
    if let Some(code) = status.code {
        (code, None)
    } else if let Some(sig) = status.signal {
        (-1, Some(format_signal(sig)))
    } else {
        (-1, None)
    }
}

fn format_signal(sig: i32) -> String {
    // Stay minimal: expose the conventional signal names hosts render.
    match sig {
        1 => "SIGHUP".into(),
        2 => "SIGINT".into(),
        3 => "SIGQUIT".into(),
        6 => "SIGABRT".into(),
        9 => "SIGKILL".into(),
        13 => "SIGPIPE".into(),
        14 => "SIGALRM".into(),
        15 => "SIGTERM".into(),
        24 => "SIGXCPU".into(),
        25 => "SIGXFSZ".into(),
        other => format!("SIG{other}"),
    }
}

/// Parse `cwd` from the request payload, validating that it is an existing
/// directory. Optional fields stay `None` so the spawned child inherits the
/// hostlib process's cwd.
pub(crate) fn parse_cwd(
    builtin: &'static str,
    raw: Option<&str>,
) -> Result<Option<PathBuf>, HostlibError> {
    let Some(raw) = raw else { return Ok(None) };
    if raw.is_empty() {
        return Ok(None);
    }
    let path = Path::new(raw);
    if !path.is_dir() {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param: "cwd",
            message: format!("not an existing directory: {raw}"),
        });
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| HostlibError::InvalidParameter {
            builtin,
            param: "cwd",
            message: format!("failed to canonicalize cwd `{raw}`: {error}"),
        })?;
    Ok(Some(canonical))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    struct RecordingKiller {
        calls: AtomicUsize,
    }

    impl RecordingKiller {
        fn calls(&self) -> usize {
            self.calls.load(AtomicOrdering::SeqCst)
        }
    }

    impl ProcessKiller for RecordingKiller {
        fn kill(&self) -> process_handle::ProcessCleanupReport {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            process_handle::ProcessCleanupReport::for_signal(Some(42), 9)
        }
    }

    fn recording_killer() -> Arc<RecordingKiller> {
        Arc::new(RecordingKiller {
            calls: AtomicUsize::new(0),
        })
    }

    #[test]
    fn inline_output_does_not_split_utf8_codepoint() {
        let (stdout, stderr) = inline_output(
            "alpha 🚀 beta".as_bytes(),
            &[],
            CaptureConfig {
                max_inline_bytes: b"alpha \xF0\x9F".len(),
                ..CaptureConfig::default()
            },
        );

        assert_eq!(stdout, "alpha ");
        assert_eq!(stderr, "");
    }

    #[test]
    fn inline_output_preserves_lossy_text_after_invalid_byte() {
        let (stdout, stderr) = inline_output(
            b"a\xffb",
            &[],
            CaptureConfig {
                max_inline_bytes: 3,
                ..CaptureConfig::default()
            },
        );

        assert_eq!(stdout, "a\u{fffd}b");
        assert_eq!(stderr, "");
    }

    #[test]
    fn parse_cwd_returns_canonical_directory() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("a").join("..");
        std::fs::create_dir_all(temp.path().join("a")).unwrap();
        let parsed = parse_cwd("test", Some(nested.to_str().unwrap()))
            .unwrap()
            .unwrap();

        assert_eq!(parsed, temp.path().canonicalize().unwrap());
    }

    #[test]
    fn drain_output_kills_immediately_when_deadline_already_elapsed() {
        let (_tx, rx) = mpsc::channel::<Vec<u8>>();
        let killer = recording_killer();
        let killer_trait: Arc<dyn ProcessKiller> = killer.clone();
        let expired_deadline = std::time::Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("subtracting one millisecond from Instant::now() should be representable");

        let result = drain_output(rx, Some(expired_deadline), false, &killer_trait);

        assert!(result.timed_out);
        assert!(!result.interrupted);
        assert_eq!(result.bytes, Vec::<u8>::new());
        assert_eq!(killer.calls(), 1);
        assert_eq!(result.process_cleanup.unwrap().root_pid, Some(42));
    }

    #[test]
    fn drain_output_collects_all_available_chunks() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        tx.send(b"alpha".to_vec()).unwrap();
        tx.send(b"beta".to_vec()).unwrap();
        drop(tx);
        let killer = recording_killer();
        let killer_trait: Arc<dyn ProcessKiller> = killer.clone();

        let result = drain_output(rx, None, false, &killer_trait);

        assert_eq!(result.bytes, b"alphabeta");
        assert!(!result.timed_out);
        assert!(!result.interrupted);
        assert!(result.process_cleanup.is_none());
        assert_eq!(killer.calls(), 0);
    }
}

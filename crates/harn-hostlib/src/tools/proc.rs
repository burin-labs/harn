//! Shared subprocess spawn / wait / timeout machinery used by every
//! `tools/run_*` and `tools/manage_packages` builtin.
//!
//! All process tools funnel through here so:
//! 1. Each spawn goes through [`harn_vm::process_sandbox`], so the active
//!    orchestration capability policy applies (Linux seccomp/landlock,
//!    macOS `sandbox-exec`, workspace-root cwd enforcement).
//! 2. Pipe drains run on background threads so >64 KB output never
//!    deadlocks `wait()`.
//! 3. Timeout enforcement is uniform: when a deadline elapses, the child
//!    is killed and `timed_out: true` is reported in the response.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use harn_vm::process_sandbox;
use harn_vm::VmValue;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::error::HostlibError;
use crate::tools::response::ResponseBuilder;

mod artifacts;

use self::artifacts::planned_artifact_paths;
pub(crate) use self::artifacts::{persist_artifacts, resolve_output_path};

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnvMode {
    InheritClean,
    Replace,
    Patch,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CaptureConfig {
    pub(crate) stdout: bool,
    pub(crate) stderr: bool,
    pub(crate) merge_stderr: bool,
    pub(crate) max_inline_bytes: usize,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            stdout: true,
            stderr: true,
            merge_stderr: false,
            max_inline_bytes: DEFAULT_MAX_INLINE_BYTES,
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
    if req.program.is_empty() {
        return Err(HostlibError::InvalidParameter {
            builtin: req.builtin,
            param: "argv",
            message: "first element of argv must be a non-empty program name".to_string(),
        });
    }

    let mut command = process_sandbox::std_command_for(&req.program, &req.args).map_err(|e| {
        HostlibError::Backend {
            builtin: req.builtin,
            message: format!("sandbox setup failed: {e:?}"),
        }
    })?;

    if let Some(cwd) = req.cwd.as_ref() {
        process_sandbox::enforce_process_cwd(cwd).map_err(|e| HostlibError::Backend {
            builtin: req.builtin,
            message: format!("sandbox cwd rejected: {e:?}"),
        })?;
        command.current_dir(cwd);
    }

    if matches!(req.env_mode, EnvMode::Replace) {
        command.env_clear();
    }
    if !req.env.is_empty() {
        for (key, value) in &req.env {
            command.env(key, value);
        }
    }

    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.stdin(if req.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });

    let command_id = next_command_id();
    let started = Instant::now();
    let started_at = now_rfc3339();
    let mut child = command.spawn().map_err(|e| {
        if let Some(violation) = process_sandbox::process_spawn_error(&e) {
            return HostlibError::Backend {
                builtin: req.builtin,
                message: format!("sandbox rejected spawn: {violation:?}"),
            };
        }
        HostlibError::Backend {
            builtin: req.builtin,
            message: format!("spawn failed: {e}"),
        }
    })?;
    let pid = Some(child.id());
    let process_group_id = child_process_group_id(child.id());

    if let Some(stdin_data) = req.stdin.as_ref() {
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            // A failed write is non-fatal — the child may have closed stdin
            // immediately. We surface the eventual exit code regardless.
            let _ = stdin.write_all(stdin_data.as_bytes());
        }
    }

    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();

    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>();
    let (err_tx, err_rx) = mpsc::channel::<Vec<u8>>();

    let stdout_thread = stdout_handle.map(|mut handle| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = handle.read_to_end(&mut buf);
            let _ = out_tx.send(buf);
        })
    });
    let stderr_thread = stderr_handle.map(|mut handle| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = handle.read_to_end(&mut buf);
            let _ = err_tx.send(buf);
        })
    });

    let (status, timed_out) = wait_with_timeout(&mut child, req.timeout);

    if let Some(t) = stdout_thread {
        let _ = t.join();
    }
    if let Some(t) = stderr_thread {
        let _ = t.join();
    }

    let stdout_bytes: Vec<u8> = out_rx.try_iter().flatten().collect();
    let stderr_bytes: Vec<u8> = err_rx.try_iter().flatten().collect();

    let ended_at = Some(now_rfc3339());

    let exited = status.is_some();
    let (exit_code, signal) = match status {
        Some(status) => decode_status(status),
        None => (-1, Some("SIGKILL".to_string())),
    };
    let command_status = if timed_out {
        CommandStatus::TimedOut
    } else if exited {
        CommandStatus::Completed
    } else {
        CommandStatus::Killed
    };
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
    })
}

pub(crate) fn build_response(
    outcome: SpawnOutcome,
    handle_id: Option<String>,
    policy_context: Option<BTreeMap<String, VmValue>>,
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
        .str("output_path", outcome.output_path.display().to_string())
        .str("stdout_path", outcome.stdout_path.display().to_string())
        .str("stderr_path", outcome.stderr_path.display().to_string())
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
    builder = match handle_id {
        Some(handle_id) => builder.str("handle_id", handle_id),
        None => builder.nil("handle_id"),
    };
    let mut sandbox = BTreeMap::new();
    sandbox.insert(
        "kind".to_string(),
        VmValue::String(Rc::from(sandbox_kind())),
    );
    sandbox.insert("enforced".to_string(), VmValue::Bool(sandbox_enforced()));
    builder = builder.dict("sandbox", sandbox);
    if let Some(policy_context) = policy_context {
        builder = builder.dict("policy_context", policy_context);
    }
    builder.build()
}

pub(crate) fn running_response(
    command_id: String,
    handle_id: String,
    pid: u32,
    process_group_id: Option<u32>,
    started_at: String,
    command_display: String,
) -> VmValue {
    let artifacts = planned_artifact_paths(&command_id);
    let mut sandbox = BTreeMap::new();
    sandbox.insert(
        "kind".to_string(),
        VmValue::String(Rc::from(sandbox_kind())),
    );
    sandbox.insert("enforced".to_string(), VmValue::Bool(sandbox_enforced()));
    ResponseBuilder::new()
        .str("command_id", command_id.clone())
        .str("status", CommandStatus::Running.as_str())
        .int("pid", pid as i64)
        .int("process_group_id", process_group_id.unwrap_or(pid) as i64)
        .str("handle_id", handle_id)
        .str("started_at", started_at)
        .nil("ended_at")
        .int("duration_ms", 0)
        .nil("exit_code")
        .nil("signal")
        .bool("timed_out", false)
        .str("stdout", "")
        .str("stderr", "")
        .str("output_path", artifacts.output_path.display().to_string())
        .str("stdout_path", artifacts.stdout_path.display().to_string())
        .str("stderr_path", artifacts.stderr_path.display().to_string())
        .int("line_count", 0)
        .int("byte_count", 0)
        .str("output_sha256", "")
        .dict("sandbox", sandbox)
        .str("audit_id", format!("audit_{command_id}"))
        .str("command", command_display.clone())
        .str("command_or_op_descriptor", command_display)
        .build()
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

pub(crate) fn child_process_group_id(pid: u32) -> Option<u32> {
    #[cfg(unix)]
    {
        extern "C" {
            fn getpgid(pid: i32) -> i32;
        }
        let pgid = unsafe { getpgid(pid as i32) };
        if pgid > 0 {
            Some(pgid as u32)
        } else {
            None
        }
    }
    #[cfg(not(unix))]
    {
        Some(pid)
    }
}

pub(crate) fn configure_background_process_group(command: &mut std::process::Command) {
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            extern "C" {
                fn setpgid(pid: i32, pgid: i32) -> i32;
            }
            if setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    #[cfg(not(unix))]
    {
        let _ = command;
    }
}

fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Option<Duration>,
) -> (Option<std::process::ExitStatus>, bool) {
    let Some(timeout) = timeout else {
        return (child.wait().ok(), false);
    };
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return (Some(status), false),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return (None, true);
                }
                // Cheap poll. Real workloads are dominated by spawn cost
                // and pipe drain, not this sleep.
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return (child.wait().ok(), false),
        }
    }
}

fn lossy_prefix(bytes: &[u8], max_inline_bytes: usize) -> String {
    let cap = bytes.len().min(max_inline_bytes);
    match std::str::from_utf8(&bytes[..cap]) {
        Ok(text) => text.to_string(),
        Err(error) => String::from_utf8_lossy(&bytes[..error.valid_up_to()]).into_owned(),
    }
}

fn sandbox_kind() -> &'static str {
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

fn sandbox_enforced() -> bool {
    harn_vm::orchestration::current_execution_policy().is_some()
}

#[cfg(unix)]
fn decode_status(status: std::process::ExitStatus) -> (i32, Option<String>) {
    use std::os::unix::process::ExitStatusExt;
    if let Some(code) = status.code() {
        (code, None)
    } else if let Some(sig) = status.signal() {
        (-1, Some(format_signal(sig)))
    } else {
        (-1, None)
    }
}

#[cfg(not(unix))]
fn decode_status(status: std::process::ExitStatus) -> (i32, Option<String>) {
    (status.code().unwrap_or(-1), None)
}

#[cfg(unix)]
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
    fn parse_cwd_returns_canonical_directory() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("a").join("..");
        std::fs::create_dir_all(temp.path().join("a")).unwrap();
        let parsed = parse_cwd("test", Some(nested.to_str().unwrap()))
            .unwrap()
            .unwrap();

        assert_eq!(parsed, temp.path().canonicalize().unwrap());
    }
}

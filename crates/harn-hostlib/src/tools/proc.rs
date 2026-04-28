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
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use harn_vm::process_sandbox;

use crate::error::HostlibError;

/// Resolved request payload for a subprocess spawn.
#[derive(Debug, Clone)]
pub(crate) struct SpawnRequest {
    pub(crate) builtin: &'static str,
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) stdin: Option<String>,
    pub(crate) timeout: Option<Duration>,
}

/// Result of running a subprocess to completion (or to the deadline).
#[derive(Debug, Clone)]
pub(crate) struct SpawnOutcome {
    pub(crate) exit_code: i32,
    pub(crate) signal: Option<String>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) duration: Duration,
    pub(crate) timed_out: bool,
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

    if !req.env.is_empty() {
        // The caller is responsible for every variable they want set; we
        // don't merge in the parent env. This
        // matches the schema (env is a complete map, not a patch).
        command.env_clear();
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

    let started = Instant::now();
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

    let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();

    let (exit_code, signal) = match status {
        Some(status) => decode_status(status),
        None => (-1, Some("SIGKILL".to_string())),
    };

    Ok(SpawnOutcome {
        exit_code,
        signal,
        stdout,
        stderr,
        duration: started.elapsed(),
        timed_out,
    })
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
    Ok(Some(path.to_path_buf()))
}

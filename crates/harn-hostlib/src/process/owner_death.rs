//! Native owner-death containment for managed background processes.
//!
//! Unix re-executes the current embedding executable as a small reaper plus a
//! process-group-leading guardian. The executable must dispatch
//! [`run_if_requested`] before its public argument parser. The supervisor
//! retains the only write end of the guardian's stdin pipe. Kernel EOF
//! therefore remains reliable even when the supervisor is killed before Rust
//! destructors or session cleanup can run, while the out-of-group reaper makes
//! guardian PGID disappearance deterministic.

#[cfg(unix)]
use std::cell::RefCell;
#[cfg(unix)]
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Child, ChildStderr, Command, ExitStatus, Stdio};

#[cfg(unix)]
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use super::{ProcessError, SpawnSpec};

/// Private argv marker handled before the public CLI parser runs.
pub const GUARDIAN_ARG: &str = "__harn-process-owner-guardian";

#[cfg(unix)]
const REQUEST_ENV: &str = "HARN_INTERNAL_PROCESS_GUARDIAN_REQUEST";
#[cfg(unix)]
const REAPER_ENV: &str = "HARN_INTERNAL_PROCESS_GUARDIAN_REAPER";

#[cfg(unix)]
thread_local! {
    static REEXEC_ARGS: RefCell<Option<Vec<OsString>>> = const { RefCell::new(None) };
}

/// Restores the previous thread-local guardian re-exec arguments on drop.
#[cfg(unix)]
#[doc(hidden)]
pub struct GuardianReexecArgsGuard {
    previous: Option<Vec<OsString>>,
}

#[cfg(unix)]
impl Drop for GuardianReexecArgsGuard {
    fn drop(&mut self) {
        REEXEC_ARGS.with(|slot| {
            *slot.borrow_mut() = self.previous.take();
        });
    }
}

/// Override the private guardian re-exec argv on this thread.
///
/// Integration tests use this to enter a libtest fixture because their
/// generated executable does not own `main`. Production leaves it unset.
#[cfg(unix)]
#[doc(hidden)]
pub fn install_guardian_reexec_args<I, S>(args: I) -> GuardianReexecArgsGuard
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let args = args.into_iter().map(Into::into).collect();
    let previous = REEXEC_ARGS.with(|slot| slot.replace(Some(args)));
    GuardianReexecArgsGuard { previous }
}

#[cfg(unix)]
#[derive(Deserialize, Serialize)]
struct PreparedCommand {
    program: Vec<u8>,
    args: Vec<Vec<u8>>,
    cwd: Option<Vec<u8>>,
    env_clear: bool,
    env: Vec<(Vec<u8>, Option<Vec<u8>>)>,
    cleanup_token: String,
}

#[cfg(unix)]
#[derive(Deserialize, Serialize)]
struct StartupMessage {
    ok: bool,
    error: Option<String>,
    guardian_pid: Option<u32>,
    pid: Option<u32>,
}

/// Build the guardian re-exec command and its inherited payload request.
#[cfg(unix)]
pub(crate) fn prepare_guardian(
    spec: &SpawnSpec,
    cleanup_token: String,
) -> Result<Command, ProcessError> {
    let mut payload_spec = spec.clone();
    payload_spec.configure_process_group = false;
    payload_spec.owner_death = super::OwnerDeathPolicy::None;
    let (payload, _) = super::real::prepare_command(&payload_spec, Some(cleanup_token.clone()))?;
    let request = PreparedCommand::from_command(
        &payload,
        spec.env_mode == super::EnvMode::Replace,
        cleanup_token,
    );
    let request = serde_json::to_string(&request)
        .map_err(|error| ProcessError::Spawn(format!("encode guardian request: {error}")))?;

    let executable = std::env::current_exe()
        .map_err(|error| ProcessError::Spawn(format!("resolve guardian executable: {error}")))?;
    let mut guardian = Command::new(executable);
    match REEXEC_ARGS.with(|slot| slot.borrow().clone()) {
        Some(args) => {
            guardian.args(args);
        }
        None => {
            guardian.arg(GUARDIAN_ARG);
        }
    }
    guardian
        .env(REQUEST_ENV, request)
        .env(REAPER_ENV, "1")
        .env_remove(harn_vm::op_interrupt::PROCESS_CLEANUP_TOKEN_ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    Ok(guardian)
}

#[cfg(unix)]
impl PreparedCommand {
    fn from_command(command: &Command, env_clear: bool, cleanup_token: String) -> Self {
        Self {
            program: os_bytes(command.get_program()),
            args: command.get_args().map(os_bytes).collect(),
            cwd: command
                .get_current_dir()
                .map(|path| os_bytes(path.as_os_str())),
            env_clear,
            env: command
                .get_envs()
                .map(|(key, value)| (os_bytes(key), value.map(os_bytes)))
                .collect(),
            cleanup_token,
        }
    }

    fn into_command(self) -> (Command, String) {
        let mut command = Command::new(OsString::from_vec(self.program));
        command.args(self.args.into_iter().map(OsString::from_vec));
        if let Some(cwd) = self.cwd {
            command.current_dir(PathBuf::from(OsString::from_vec(cwd)));
        }
        if self.env_clear {
            command.env_clear();
        }
        for (key, value) in self.env {
            let key = OsString::from_vec(key);
            match value {
                Some(value) => {
                    command.env(key, OsString::from_vec(value));
                }
                None => {
                    command.env_remove(key);
                }
            }
        }
        command
            .env_remove(REQUEST_ENV)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        (command, self.cleanup_token)
    }
}

#[cfg(unix)]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    value.as_bytes().to_vec()
}

/// Read the guardian's spawn handshake without consuming payload stderr.
#[cfg(unix)]
pub(crate) fn await_startup(child: &mut Child) -> Result<(ChildStderr, u32, u32), ProcessError> {
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| ProcessError::Spawn("guardian stderr pipe missing".to_string()))?;
    let mut line = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        match stderr.read_exact(&mut byte) {
            Ok(()) if byte[0] == b'\n' => break,
            Ok(()) => {
                line.push(byte[0]);
                if line.len() > 64 * 1024 {
                    return Err(ProcessError::Spawn(
                        "guardian startup response exceeded 64 KiB".to_string(),
                    ));
                }
            }
            Err(error) => {
                let _ = child.wait();
                return Err(ProcessError::Spawn(format!(
                    "guardian exited before payload startup: {error}"
                )));
            }
        }
    }
    let message: StartupMessage = serde_json::from_slice(&line)
        .map_err(|error| ProcessError::Spawn(format!("decode guardian startup: {error}")))?;
    if message.ok {
        let guardian_pid = message.guardian_pid.ok_or_else(|| {
            ProcessError::Spawn("guardian startup response omitted guardian pid".to_string())
        })?;
        let pid = message.pid.ok_or_else(|| {
            ProcessError::Spawn("guardian startup response omitted payload pid".to_string())
        })?;
        Ok((stderr, guardian_pid, pid))
    } else {
        let _ = child.wait();
        Err(ProcessError::Spawn(message.error.unwrap_or_else(|| {
            "guardian could not launch payload".to_string()
        })))
    }
}

/// Run the guardian payload when the private request environment is present.
///
/// This is public only so a re-executed integration-test fixture can enter the
/// same native guardian path as the shipped `harn` executable.
#[cfg(unix)]
#[doc(hidden)]
pub fn run_guardian_from_env() -> io::Result<()> {
    if std::env::var_os(REAPER_ENV).is_some() {
        run_guardian_reaper();
    }
    let raw = std::env::var(REQUEST_ENV)
        .map_err(|_| io::Error::other("guardian request environment is missing"))?;
    let request: PreparedCommand = serde_json::from_str(&raw)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let (mut payload_command, cleanup_token) = request.into_command();
    configure_child_reaper()?;
    let mut payload = match payload_command.spawn() {
        Ok(payload) => payload,
        Err(error) => {
            write_startup(StartupMessage {
                ok: false,
                error: Some(error.to_string()),
                guardian_pid: None,
                pid: None,
            })?;
            return Err(error);
        }
    };
    let payload_pid = payload.id();
    write_startup(StartupMessage {
        ok: true,
        error: None,
        guardian_pid: Some(std::process::id()),
        pid: Some(payload_pid),
    })?;

    let stdout = payload.stdout.take();
    let stderr = payload.stderr.take();
    let (event_tx, event_rx) = std::sync::mpsc::channel();

    if let Some(mut stdout) = stdout {
        let event_tx = event_tx.clone();
        std::thread::spawn(move || {
            let _ = io::copy(&mut stdout, &mut io::stdout());
            let _ = event_tx.send(GuardianEvent::OutputClosed);
        });
    }
    if let Some(mut stderr) = stderr {
        let event_tx = event_tx.clone();
        std::thread::spawn(move || {
            let _ = io::copy(&mut stderr, &mut io::stderr());
            let _ = event_tx.send(GuardianEvent::OutputClosed);
        });
    }
    {
        let event_tx = event_tx.clone();
        std::thread::spawn(move || {
            let status = payload.wait();
            let _ = event_tx.send(GuardianEvent::PayloadExited(status));
        });
    }
    {
        let event_tx = event_tx.clone();
        std::thread::spawn(move || {
            let mut stdin = io::stdin();
            let mut sink = [0_u8; 256];
            loop {
                match stdin.read(&mut sink) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            let _ = event_tx.send(GuardianEvent::OwnerClosed);
        });
    }
    drop(event_tx);

    let mut payload_status = None;
    let mut open_outputs = 2_u8;
    while let Ok(event) = event_rx.recv() {
        match event {
            GuardianEvent::OwnerClosed => {
                let _ =
                    harn_vm::op_interrupt::signal_pid_tree_and_token_preserving_group_with_report(
                        payload_pid,
                        Some(&cleanup_token),
                        unsafe { libc::getpgrp() as u32 },
                        libc::SIGKILL,
                    );
                wait_for_payload_exit(&event_rx, &mut payload_status)?;
                reap_adopted_children()?;
                unsafe {
                    libc::kill(-libc::getpgrp(), libc::SIGKILL);
                }
                return Err(io::Error::other("guardian process group survived SIGKILL"));
            }
            GuardianEvent::PayloadExited(status) => payload_status = Some(status?),
            GuardianEvent::OutputClosed => open_outputs = open_outputs.saturating_sub(1),
        }
        if let Some(status) = payload_status.filter(|_| open_outputs == 0) {
            propagate_exit(status);
        }
    }
    Err(io::Error::other(
        "guardian event channels closed unexpectedly",
    ))
}

#[cfg(unix)]
fn run_guardian_reaper() -> ! {
    let executable = std::env::current_exe().unwrap_or_else(|error| {
        eprintln!("resolve guardian executable: {error}");
        std::process::exit(1);
    });
    let mut guardian = Command::new(executable);
    guardian
        .args(std::env::args_os().skip(1))
        .env_remove(REAPER_ENV)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .process_group(0);
    let mut guardian = guardian.spawn().unwrap_or_else(|error| {
        eprintln!("spawn process guardian: {error}");
        std::process::exit(1);
    });
    match guardian.wait() {
        Ok(status) => propagate_exit(status),
        Err(error) => {
            eprintln!("reap process guardian: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "linux")]
fn configure_child_reaper() -> io::Result<()> {
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn configure_child_reaper() -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn wait_for_payload_exit(
    events: &std::sync::mpsc::Receiver<GuardianEvent>,
    payload_status: &mut Option<ExitStatus>,
) -> io::Result<()> {
    while payload_status.is_none() {
        match events.recv() {
            Ok(GuardianEvent::PayloadExited(status)) => *payload_status = Some(status?),
            Ok(GuardianEvent::OutputClosed | GuardianEvent::OwnerClosed) => {}
            Err(_) => {
                return Err(io::Error::other(
                    "guardian events closed before payload exit",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn reap_adopted_children() -> io::Result<()> {
    loop {
        let result = unsafe { libc::waitpid(-1, std::ptr::null_mut(), 0) };
        if result > 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::ECHILD) => return Ok(()),
            _ => return Err(error),
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn reap_adopted_children() -> io::Result<()> {
    Ok(())
}

/// Whether this process carries a private guardian payload request.
#[cfg(unix)]
#[doc(hidden)]
pub fn guardian_requested() -> bool {
    std::env::var_os(REQUEST_ENV).is_some()
}

#[cfg(unix)]
enum GuardianEvent {
    OwnerClosed,
    PayloadExited(io::Result<ExitStatus>),
    OutputClosed,
}

#[cfg(unix)]
fn write_startup(message: StartupMessage) -> io::Result<()> {
    let mut stderr = io::stderr();
    serde_json::to_writer(&mut stderr, &message)?;
    stderr.write_all(b"\n")?;
    stderr.flush()
}

#[cfg(unix)]
fn propagate_exit(status: ExitStatus) -> ! {
    if let Some(code) = status.code() {
        std::process::exit(code);
    }
    if let Some(signal) = status.signal() {
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
        std::process::exit(128 + signal);
    }
    std::process::exit(1);
}

/// Enter the private guardian mode before public CLI parsing.
///
/// Executables embedding `harn-hostlib` process tools must call this at the
/// beginning of `main`, before inspecting or rejecting command-line arguments.
/// Unix owner-death containment re-executes the embedding executable with a
/// private argument; omitting this dispatch makes contained process startup
/// fail. The function returns `false` for normal invocations and on platforms
/// that do not use the re-exec guardian. Guardian invocations do not return.
#[cfg(unix)]
pub fn run_if_requested() -> bool {
    if std::env::args_os().nth(1).as_deref() != Some(OsStr::new(GUARDIAN_ARG)) {
        return false;
    }
    if let Err(error) = run_guardian_from_env() {
        eprintln!("harn process guardian failed: {error}");
        std::process::exit(1);
    }
    unreachable!("guardian execution always exits")
}

/// Unix alone uses the re-exec guardian; Windows uses a Job Object.
#[cfg(not(unix))]
pub fn run_if_requested() -> bool {
    false
}

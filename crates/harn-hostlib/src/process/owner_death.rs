//! Native owner-death containment for managed background processes.
//!
//! Unix re-executes the current embedding executable as a small reaper plus a
//! process-group-leading guardian. The executable must dispatch
//! [`run_if_requested`] before its public argument parser. The supervisor
//! holds the write end of the reaper's stdin pipe, and the reaper relays it to
//! the guardian's stdin. Kernel EOF therefore arrives even when the supervisor
//! is killed before Rust destructors or session cleanup can run. The reaper is
//! the supervisor's direct child, so it also sees the supervisor exit as a
//! changed parent pid and closes the relay then: a process that inherited a
//! copy of the supervisor's write end cannot keep the guardian alive. The
//! out-of-group reaper also makes guardian PGID disappearance deterministic.

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
use std::process::{Child, ChildStderr, ChildStdin, Command, ExitStatus, Stdio};

#[cfg(unix)]
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use super::{ProcessError, SpawnSpec};

/// Private argv marker handled before the public CLI parser runs.
pub const GUARDIAN_ARG: &str = "__harn-process-owner-guardian";

#[cfg(unix)]
const MODE_ENV: &str = "HARN_INTERNAL_PROCESS_GUARDIAN_MODE";
#[cfg(unix)]
const PIPE_MODE: &str = "request-pipe-v1";
#[cfg(unix)]
const REAPER_ENV: &str = "HARN_INTERNAL_PROCESS_GUARDIAN_REAPER";
/// The supervisor's pid, which the reaper compares with its parent pid.
#[cfg(unix)]
const OWNER_PID_ENV: &str = "HARN_INTERNAL_PROCESS_GUARDIAN_OWNER_PID";
/// How often the reaper checks that its supervisor is still its parent.
#[cfg(unix)]
const OWNER_POLL_INTERVAL_MS: libc::c_int = 200;
#[cfg(unix)]
const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

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

/// The descriptor number the populated Landlock ruleset arrives on.
///
/// Fixed rather than negotiated because the guardian learns it before it can
/// read anything: the request that would carry a negotiated number arrives on
/// stdin, and stdin is already the liveness lease.
#[cfg(all(unix, target_os = "linux"))]
const RULESET_FD: std::os::fd::RawFd = 3;

/// The confinement the guardian must enter on the payload's behalf.
///
/// The rest of [`PreparedCommand`] is a lossy projection of a `Command`: it
/// keeps what a program, its arguments, a directory and an environment can say,
/// and drops everything else. Confinement used to be in the "everything else",
/// because this backend installs it from a `pre_exec` callback and a callback
/// cannot cross an `exec`. The payload then ran unconfined while every step
/// reported success.
///
/// Direct payloads carry compiled seccomp bytes and an inherited ruleset.
/// Namespace helpers already carry seccomp in argv and must inherit the
/// ruleset without entering it until namespace setup finishes.
#[cfg(unix)]
#[derive(Deserialize, Serialize)]
enum GuardianConfinement {
    BeforeExec { seccomp: Vec<u8>, ruleset: bool },
    AfterNamespace { ruleset: bool },
    Bubblewrap { descriptors: Vec<i32> },
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pinned_verifier_descriptors: Vec<i32>,
    /// Absent means this run confines no child process at all, which is the
    /// same answer the direct spawn path acts on. It never means "confinement
    /// was wanted and could not be built": that is an error at the seam that
    /// built it, and it is returned as one.
    #[serde(default)]
    confinement: Option<GuardianConfinement>,
}

#[cfg(unix)]
#[derive(Deserialize, Serialize)]
struct StartupMessage {
    ok: bool,
    #[serde(default)]
    spawn_not_found: bool,
    error: Option<String>,
    guardian_pid: Option<u32>,
    pid: Option<u32>,
}

/// Build the guardian re-exec command and its private pipe request.
#[cfg(unix)]
pub(crate) struct PreparedGuardian {
    pub(crate) command: Command,
    pub(crate) request: Vec<u8>,
    pub(crate) missing_program: Option<String>,
    pub(crate) verifier_witness: Option<harn_vm::verifier_provenance::VerifierExecutionWitness>,
}

pub(crate) fn prepare_guardian(
    spec: &SpawnSpec,
    cleanup_token: String,
) -> Result<PreparedGuardian, ProcessError> {
    super::real::validate_program(spec)?;
    let mut payload_spec = spec.clone();
    payload_spec.configure_process_group = false;
    payload_spec.owner_death = super::OwnerDeathPolicy::None;
    #[cfg(target_os = "linux")]
    let pinned = super::program_lookup::pinned_verifier(&payload_spec)?;
    #[cfg(target_os = "linux")]
    let (prepared, confinement) = {
        let (command, env_closed, confinement) = harn_vm::process_sandbox::command_for_reexec(
            pinned
                .as_ref()
                .map_or(payload_spec.program.as_str(), |value| {
                    value.program.as_str()
                }),
            pinned
                .as_ref()
                .map_or(payload_spec.args.as_slice(), |value| value.args.as_slice()),
            RULESET_FD,
        )
        .map_err(ProcessError::sandbox_setup)?;
        let prepared = super::real::prepare_command_from(
            &payload_spec,
            Some(cleanup_token.clone()),
            (command, env_closed),
        )?;
        (prepared, confinement.map(TransferredConfinement))
    };
    #[cfg(not(target_os = "linux"))]
    let (prepared, confinement) = (
        super::real::prepare_command(&payload_spec, Some(cleanup_token.clone()))?,
        build_confinement(&payload_spec.program)?,
    );
    #[cfg(target_os = "linux")]
    let source_verifier_bound = pinned.is_some();
    #[cfg(not(target_os = "linux"))]
    let source_verifier_bound = false;
    let missing_program = if source_verifier_bound {
        None
    } else {
        super::program_lookup::missing_program(
            &payload_spec,
            &prepared.command,
            prepared.env_cleared,
        )
    };
    let mut payload = prepared.command;
    payload.env(
        harn_vm::op_interrupt::PROCESS_OWNER_TOKEN_ENV,
        &cleanup_token,
    );
    // Whether the payload's environment was CLEARED, not which mode was asked
    // for. An inheriting mode under a session policy is cleared and rebuilt
    // from the session's resolved set; sending the mode instead dropped that
    // clear in transfer, and the guardian's own environment reached the child
    // behind the explicit entries.
    let request = PreparedCommand::from_command(
        &payload,
        prepared.env_cleared,
        cleanup_token.clone(),
        confinement.as_ref().map(TransferredConfinement::request),
    );
    #[cfg(target_os = "linux")]
    let mut request = request;
    #[cfg(target_os = "linux")]
    if let Some(pinned) = pinned.as_ref() {
        if payload.get_program() != std::ffi::OsStr::new(&pinned.program) {
            return Err(ProcessError::Spawn(
                "isolated source verifier guardian wrapping is unmeasured".to_string(),
            ));
        }
        harn_vm::verifier_provenance::validate_pinned_environment(&payload, prepared.env_cleared)
            .map_err(ProcessError::Spawn)?;
        request.pinned_verifier_descriptors = pinned.descriptors.numbers();
    }
    let request = serde_json::to_vec(&request)
        .map_err(|error| ProcessError::Spawn(format!("encode guardian request: {error}")))?;

    let executable = std::env::current_exe()
        .map_err(|error| ProcessError::Spawn(format!("resolve guardian executable: {error}")))?;
    harn_vm::op_interrupt::initialize_process_owner_group_journal(&cleanup_token)
        .map_err(|error| ProcessError::Spawn(format!("create owner process journal: {error}")))?;
    let mut guardian = Command::new(executable);
    match REEXEC_ARGS.with(|slot| slot.borrow().clone()) {
        Some(args) => {
            guardian.args(args);
        }
        None => {
            guardian.arg(GUARDIAN_ARG);
        }
    }
    strip_sensitive_parent_env(&mut guardian, std::env::vars_os());
    guardian
        .env(MODE_ENV, PIPE_MODE)
        .env(REAPER_ENV, "1")
        .env(OWNER_PID_ENV, std::process::id().to_string())
        .env_remove(harn_vm::op_interrupt::PROCESS_CLEANUP_TOKEN_ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(confinement) = confinement {
        confinement.hand_to(&mut guardian);
    }
    #[cfg(target_os = "linux")]
    let witness = if let Some(pinned) = pinned {
        pinned.descriptors.attach(&mut guardian);
        Some(pinned.witness)
    } else {
        None
    };
    #[cfg(not(target_os = "linux"))]
    let witness = None;
    Ok(PreparedGuardian {
        command: guardian,
        request,
        missing_program,
        verifier_witness: witness,
    })
}

/// The parent's side of the handover.
///
/// A confinement crosses an `exec` as a descriptor plus a byte string, and only
/// the descriptor needs help: the kernel opens a Landlock ruleset close-on-exec,
/// so it has to be placed on an agreed number with that flag cleared, and it has
/// to stay open until the guardian is spawned. Owning it here does both — the
/// value lives on the guardian `Command`, which outlives the spawn.
#[cfg(target_os = "linux")]
struct TransferredConfinement(harn_vm::process_sandbox::ReexecConfinement);

#[cfg(target_os = "linux")]
impl TransferredConfinement {
    fn request(&self) -> GuardianConfinement {
        use harn_vm::process_sandbox::ReexecConfinement;
        match &self.0 {
            ReexecConfinement::BeforeExec(inner) => GuardianConfinement::BeforeExec {
                seccomp: inner.seccomp_bytes(),
                ruleset: inner.ruleset_fd().is_some(),
            },
            ReexecConfinement::AfterNamespace(inner) => GuardianConfinement::AfterNamespace {
                ruleset: inner.ruleset_fd().is_some(),
            },
            ReexecConfinement::Bubblewrap(descriptors) => GuardianConfinement::Bubblewrap {
                descriptors: descriptors.numbers(),
            },
        }
    }

    fn hand_to(self, guardian: &mut Command) {
        use harn_vm::process_sandbox::ReexecConfinement;
        let inner = match self.0 {
            ReexecConfinement::BeforeExec(inner) | ReexecConfinement::AfterNamespace(inner) => {
                inner
            }
            ReexecConfinement::Bubblewrap(descriptors) => {
                descriptors.attach(guardian);
                return;
            }
        };
        let Some(ruleset) = inner.into_ruleset_fd() else {
            return;
        };
        // SAFETY: `dup2` and `fcntl` are async-signal-safe, which is what
        // `pre_exec` requires. The guard closes the descriptor if it is still
        // ours when the command is dropped, so an unspawned command leaks
        // nothing.
        let guard = RulesetDescriptor(ruleset);
        unsafe {
            guardian.pre_exec(move || {
                // `guard.raw()` and not `guard.0`. An edition-2021 closure
                // captures the FIELDS it names, so naming the descriptor alone
                // captures a `RawFd`, which is `Copy`, and leaves the guard
                // itself behind to drop at the end of this function — closing
                // the ruleset before the child that has to enter it exists. A
                // method call captures the whole guard, which is the lifetime
                // this handover depends on.
                let held = guard.raw();
                if held == RULESET_FD {
                    // Already on the agreed number: it only needs the
                    // close-on-exec flag cleared to survive the `exec`.
                    if libc::fcntl(held, libc::F_SETFD, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                } else if libc::dup2(held, RULESET_FD) < 0 {
                    // `dup2` clears close-on-exec on the new descriptor.
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

/// Owns the ruleset descriptor for as long as the guardian command does.
#[cfg(target_os = "linux")]
struct RulesetDescriptor(std::os::fd::RawFd);

#[cfg(target_os = "linux")]
impl RulesetDescriptor {
    fn raw(&self) -> std::os::fd::RawFd {
        self.0
    }
}

#[cfg(target_os = "linux")]
impl Drop for RulesetDescriptor {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.0);
        }
    }
}

/// Build the confinement this spawn must carry, or state that it carries none.
///
/// `Ok(None)` means the run confines no child. It never means confinement was
/// wanted and could not be built: that is an error, and it is returned as one,
/// so no caller can read a refusal as an absent request.
/// Other platforms wrap the payload's argv, and the request carries argv, so
/// their confinement crosses the handover on its own.
#[cfg(all(unix, not(target_os = "linux")))]
fn build_confinement(_program: &str) -> Result<Option<TransferredConfinement>, ProcessError> {
    Ok(None)
}

/// The no-op the non-Linux paths type-check against.
#[cfg(all(unix, not(target_os = "linux")))]
struct TransferredConfinement;

#[cfg(all(unix, not(target_os = "linux")))]
impl TransferredConfinement {
    fn request(&self) -> GuardianConfinement {
        unreachable!("no platform but Linux builds a transferred confinement")
    }

    fn hand_to(self, _guardian: &mut Command) {}
}

#[cfg(unix)]
fn strip_sensitive_parent_env<I>(guardian: &mut Command, parent_env: I)
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    for (key, _) in parent_env {
        if key
            .to_str()
            .is_some_and(super::handle::is_sensitive_env_name)
            || harn_vm::security::is_trusted_setup_control(&key)
        {
            guardian.env_remove(key);
        }
    }
}

/// Send the prepared command before retaining the same pipe as the owner's
/// liveness lease. The payload may contain credentials, so it must never be
/// placed in argv or the guardian environment.
#[cfg(unix)]
pub(crate) fn write_request(pipe: &mut ChildStdin, request: &[u8]) -> Result<(), ProcessError> {
    if request.len() > MAX_REQUEST_BYTES {
        return Err(ProcessError::Spawn(format!(
            "guardian request exceeded {MAX_REQUEST_BYTES} bytes"
        )));
    }
    pipe.write_all(request)
        .and_then(|()| pipe.write_all(b"\n"))
        .and_then(|()| pipe.flush())
        .map_err(|error| ProcessError::Spawn(format!("write guardian request: {error}")))
}

#[cfg(unix)]
impl PreparedCommand {
    fn from_command(
        command: &Command,
        env_clear: bool,
        cleanup_token: String,
        confinement: Option<GuardianConfinement>,
    ) -> Self {
        Self {
            pinned_verifier_descriptors: Vec::new(),
            confinement,
            program: os_bytes(command.get_program()),
            args: command.get_args().map(os_bytes).collect(),
            cwd: command
                .get_current_dir()
                .map(|path| os_bytes(path.as_os_str())),
            env_clear,
            // The guardian's loader must not see these inherited controls.
            // Preserve them for the confined payload through its existing
            // private request, then apply explicit entries and removals last.
            env: payload_environment(command, env_clear, std::env::vars_os()),
            cleanup_token,
        }
    }

    fn into_command(self) -> io::Result<(Command, String)> {
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
            .env_remove(MODE_ENV)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_confinement(&mut command, self.confinement)?;
        if !self.pinned_verifier_descriptors.is_empty() {
            #[cfg(target_os = "linux")]
            {
                harn_vm::verifier_provenance::validate_pinned_environment(&command, self.env_clear)
                    .map_err(io::Error::other)?;
                // The existing host-only request pipe transfers ownership of these descriptors.
                unsafe {
                    harn_vm::process_sandbox::DescriptorTransfer::inherited(
                        self.pinned_verifier_descriptors,
                    )?
                }
                .attach(&mut command);
            }
            #[cfg(not(target_os = "linux"))]
            return Err(io::Error::other(
                "isolated source verifier descriptor transport is unmeasured on this platform",
            ));
        }
        Ok((command, self.cleanup_token))
    }
}

#[cfg(unix)]
fn payload_environment(
    command: &Command,
    env_clear: bool,
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(Vec<u8>, Option<Vec<u8>>)> {
    inherited
        .into_iter()
        .filter(|(key, _)| !env_clear && harn_vm::security::is_trusted_setup_control(key))
        .map(|(key, value)| (os_bytes(&key), Some(os_bytes(&value))))
        .chain(
            command
                .get_envs()
                .map(|(key, value)| (os_bytes(key), value.map(os_bytes))),
        )
        .collect()
}

/// Attach the transferred confinement to the payload spawn, or refuse.
///
/// Built here, before the fork, because rebuilding allocates and the `pre_exec`
/// side may not. The closure is then two raw syscalls.
///
/// Fail-closed by construction: every path that cannot install what it was
/// handed returns an error, and the guardian answers the startup handshake with
/// it instead of spawning. A confinement that could not be applied must never
/// come out as a payload that merely ran.
#[cfg(target_os = "linux")]
fn apply_confinement(
    command: &mut Command,
    confinement: Option<GuardianConfinement>,
) -> io::Result<()> {
    let Some(confinement) = confinement else {
        return Ok(());
    };
    let (seccomp, ruleset) = match confinement {
        GuardianConfinement::Bubblewrap { descriptors } => {
            // SAFETY: the trusted prepared launch transferred each owned fd
            // under the exact number named in its wrapper arguments. The
            // decoder validates that all names are distinct and still open.
            let transferred =
                unsafe { harn_vm::process_sandbox::DescriptorTransfer::inherited(descriptors)? };
            transferred.attach(command);
            return Ok(());
        }
        GuardianConfinement::BeforeExec { seccomp, ruleset } => (seccomp, ruleset),
        GuardianConfinement::AfterNamespace { ruleset } => {
            let transferable = harn_vm::process_sandbox::TransferableConfinement::from_parts(
                ruleset.then_some(RULESET_FD),
                &[],
            )?;
            harn_vm::process_sandbox::keep_ruleset_across_exec(command, transferable);
            return Ok(());
        }
    };
    let ruleset = ruleset.then_some(RULESET_FD);
    let transferable =
        harn_vm::process_sandbox::TransferableConfinement::from_parts(ruleset, &seccomp)?;
    // SAFETY: `enter` is two raw syscalls for Landlock and one for seccomp,
    // with no allocation, locking, or I/O, which is what `pre_exec` requires.
    unsafe {
        command.pre_exec(move || transferable.enter());
    }
    Ok(())
}

/// Every other platform puts its confinement in the spawn's argv, which the
/// request already carries, so there is nothing to reattach here. Being handed
/// one anyway means the two sides disagree about who confines, and the safe
/// reading of that is a refusal rather than an unconfined payload.
#[cfg(all(unix, not(target_os = "linux")))]
fn apply_confinement(
    _command: &mut Command,
    confinement: Option<GuardianConfinement>,
) -> io::Result<()> {
    if confinement.is_some() {
        return Err(io::Error::other(
            "guardian was handed a transferred confinement on a platform whose backend carries its own",
        ));
    }
    Ok(())
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
        let error = message
            .error
            .unwrap_or_else(|| "guardian could not launch payload".to_string());
        if message.spawn_not_found {
            Err(ProcessError::SpawnIo {
                kind: "not_found",
                message: error,
            })
        } else {
            Err(ProcessError::Spawn(error))
        }
    }
}

/// Run the guardian payload when the private request pipe is active.
///
/// This is public only so a re-executed integration-test fixture can enter the
/// same native guardian path as the shipped `harn` executable.
#[cfg(unix)]
#[doc(hidden)]
pub fn run_guardian_from_pipe() -> io::Result<()> {
    if std::env::var_os(REAPER_ENV).is_some() {
        run_guardian_reaper();
    }
    if std::env::var(MODE_ENV).as_deref() != Ok(PIPE_MODE) {
        return Err(io::Error::other("guardian request pipe is not active"));
    }
    let raw = read_request()?;
    let request: PreparedCommand = serde_json::from_slice(&raw)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let (mut payload_command, cleanup_token) = match request.into_command() {
        Ok(prepared) => prepared,
        Err(error) => {
            write_startup(StartupMessage {
                ok: false,
                spawn_not_found: false,
                error: Some(format!("guardian could not confine the payload: {error}")),
                guardian_pid: None,
                pid: None,
            })?;
            return Err(error);
        }
    };
    let _journal_cleanup = OwnerJournalCleanup(cleanup_token.clone());
    configure_child_reaper()?;
    let mut payload = match payload_command.spawn() {
        Ok(payload) => payload,
        Err(error) => {
            write_startup(StartupMessage {
                ok: false,
                spawn_not_found: error.kind() == io::ErrorKind::NotFound,
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
        spawn_not_found: false,
        error: None,
        guardian_pid: Some(std::process::id()),
        pid: Some(payload_pid),
    })?;

    let stdout = payload.stdout.take();
    let stderr = payload.stderr.take();
    let (event_tx, event_rx) = std::sync::mpsc::channel();

    if let Some(mut stdout) = stdout {
        let event_tx = event_tx.clone();
        harn_parser::runtime_stack::spawn(move || {
            let _ = io::copy(&mut stdout, &mut io::stdout());
            let _ = event_tx.send(GuardianEvent::OutputClosed);
        });
    }
    if let Some(mut stderr) = stderr {
        let event_tx = event_tx.clone();
        harn_parser::runtime_stack::spawn(move || {
            let _ = io::copy(&mut stderr, &mut io::stderr());
            let _ = event_tx.send(GuardianEvent::OutputClosed);
        });
    }
    {
        let event_tx = event_tx.clone();
        harn_parser::runtime_stack::spawn(move || {
            let status = wait_for_payload_while_reaping_adopted(payload);
            let _ = event_tx.send(GuardianEvent::PayloadExited(status));
        });
    }
    {
        let event_tx = event_tx.clone();
        harn_parser::runtime_stack::spawn(move || {
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
                harn_vm::op_interrupt::remove_process_owner_group_journal(&cleanup_token);
                unsafe {
                    libc::kill(-libc::getpgrp(), libc::SIGKILL);
                }
                return Err(io::Error::other("guardian process group survived SIGKILL"));
            }
            GuardianEvent::PayloadExited(status) => {
                payload_status = Some(status?);
                let _ =
                    harn_vm::op_interrupt::signal_pid_tree_and_token_preserving_group_with_report(
                        payload_pid,
                        Some(&cleanup_token),
                        unsafe { libc::getpgrp() as u32 },
                        libc::SIGKILL,
                    );
                reap_adopted_children()?;
                let survivors = harn_vm::op_interrupt::process_owner_survivors(&cleanup_token);
                if !survivors.is_empty() {
                    let guardian_pid = std::process::id();
                    let guardian_pgid = unsafe { libc::getpgrp() };
                    let summary = survivors
                        .iter()
                        .map(|process| {
                            let process_group = unsafe { libc::getpgid(process.pid as i32) };
                            let process_group = if process_group < 0 {
                                "<unknown>".to_string()
                            } else {
                                process_group.to_string()
                            };
                            format!(
                                "pid={} parent={} pgid={} command={}",
                                process.pid,
                                process
                                    .parent_pid
                                    .map_or_else(|| "<unknown>".to_string(), |pid| pid.to_string()),
                                process_group,
                                process.command_name.as_deref().unwrap_or("<unknown>")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(io::Error::other(format!(
                        "owner-death guardian pid={guardian_pid} pgid={guardian_pgid} left helper \
                         processes alive after cleanup: {summary}"
                    )));
                }
            }
            GuardianEvent::OutputClosed => open_outputs = open_outputs.saturating_sub(1),
        }
        if let Some(status) = payload_status.filter(|_| open_outputs == 0) {
            harn_vm::op_interrupt::remove_process_owner_group_journal(&cleanup_token);
            propagate_exit(status);
        }
    }
    harn_vm::op_interrupt::remove_process_owner_group_journal(&cleanup_token);
    Err(io::Error::other(
        "guardian event channels closed unexpectedly",
    ))
}

/// Compatibility name for embedders that entered the hidden guardian helper
/// directly. The request now comes from stdin, not the environment.
#[cfg(unix)]
#[doc(hidden)]
#[deprecated(note = "use run_guardian_from_pipe")]
pub fn run_guardian_from_env() -> io::Result<()> {
    run_guardian_from_pipe()
}

#[cfg(unix)]
fn read_request() -> io::Result<Vec<u8>> {
    let mut stdin = io::stdin();
    let mut request = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        stdin.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            return Ok(request);
        }
        request.push(byte[0]);
        if request.len() > MAX_REQUEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("guardian request exceeded {MAX_REQUEST_BYTES} bytes"),
            ));
        }
    }
}

#[cfg(unix)]
fn run_guardian_reaper() -> ! {
    // Before anything else, so a supervisor that is already gone shows up as
    // a parent pid that no longer matches.
    let owner = std::env::var(OWNER_PID_ENV)
        .ok()
        .and_then(|pid| pid.parse::<libc::pid_t>().ok())
        .unwrap_or_else(|| unsafe { libc::getppid() });
    let executable = std::env::current_exe().unwrap_or_else(|error| {
        eprintln!("resolve guardian executable: {error}");
        std::process::exit(1);
    });
    // Created while this process is still single-threaded, so no concurrent
    // spawn can inherit the write end before it is close-on-exec.
    let (relay_reader, relay_writer) = io::pipe().unwrap_or_else(|error| {
        eprintln!("create guardian liveness relay: {error}");
        std::process::exit(1);
    });
    let mut guardian = {
        let mut guardian = Command::new(executable);
        guardian
            .args(std::env::args_os().skip(1))
            .env_remove(REAPER_ENV)
            .env_remove(OWNER_PID_ENV)
            .stdin(Stdio::from(relay_reader))
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .process_group(0);
        guardian.spawn().unwrap_or_else(|error| {
            eprintln!("spawn process guardian: {error}");
            std::process::exit(1);
        })
    };
    harn_parser::runtime_stack::spawn(move || relay_owner_liveness(owner, relay_writer));
    match guardian.wait() {
        Ok(status) => propagate_exit(status),
        Err(error) => {
            eprintln!("reap process guardian: {error}");
            std::process::exit(1);
        }
    }
}

/// Copy the supervisor's liveness pipe to the guardian until the supervisor
/// is gone, then close the guardian's end.
///
/// EOF on the supervisor's pipe is not enough on its own. A process the
/// supervisor spawned from another thread while that pipe was still
/// inheritable holds a copy of its write end, and the pipe then never closes;
/// Rust sets close-on-exec separately from `pipe()` on macOS, so that window is
/// real. This process is the supervisor's direct child, so the supervisor's
/// exit also shows up as a changed parent pid, which no inherited descriptor
/// can hide and a reused pid cannot fake.
#[cfg(unix)]
fn relay_owner_liveness(owner: libc::pid_t, mut guardian: io::PipeWriter) {
    let mut buffer = [0_u8; 4096];
    while unsafe { libc::getppid() } == owner {
        let mut poll_fd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        match unsafe { libc::poll(&raw mut poll_fd, 1, OWNER_POLL_INTERVAL_MS) } {
            0 => continue,
            ready if ready < 0 => {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            _ => {}
        }
        let read =
            unsafe { libc::read(libc::STDIN_FILENO, buffer.as_mut_ptr().cast(), buffer.len()) };
        match read {
            0 => return,
            read if read > 0 => {
                if guardian.write_all(&buffer[..read as usize]).is_err() {
                    return;
                }
            }
            _ => {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
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
fn wait_for_payload_while_reaping_adopted(payload: Child) -> io::Result<ExitStatus> {
    let payload_pid = payload.id() as libc::pid_t;
    // Keep the Child handle alive while waitpid(2) owns reaping. On Linux the
    // guardian is a subreaper, so detached helpers become its direct children.
    // Reaping all children here prevents successfully terminated helpers from
    // remaining as zombies until the conformance payload itself exits.
    let _payload = payload;
    loop {
        let mut status = 0;
        let waited = unsafe { libc::waitpid(-1, &raw mut status, 0) };
        if waited == payload_pid {
            return Ok(ExitStatus::from_raw(status));
        }
        if waited > 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            _ => return Err(error),
        }
    }
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

/// Whether this process carries a private guardian request pipe marker.
#[cfg(unix)]
#[doc(hidden)]
pub fn guardian_requested() -> bool {
    std::env::var(MODE_ENV).as_deref() == Ok(PIPE_MODE)
}

#[cfg(unix)]
struct OwnerJournalCleanup(String);

#[cfg(unix)]
impl Drop for OwnerJournalCleanup {
    fn drop(&mut self) {
        harn_vm::op_interrupt::remove_process_owner_group_journal(&self.0);
    }
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
    if let Err(error) = run_guardian_from_pipe() {
        eprintln!("harn process guardian failed: {error}");
        std::process::exit(1);
    }
    unreachable!("guardian execution always exits")
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn guardian_preserves_payload_loader_controls_without_exposing_them_to_setup() {
        let inherited = || vec![(OsString::from("LD_BIND_NOW"), OsString::from("1"))];
        let mut command = Command::new("/bin/true");
        let expected = (b"LD_BIND_NOW".to_vec(), Some(b"1".to_vec()));
        assert_eq!(
            payload_environment(&command, false, inherited()),
            vec![expected]
        );
        assert!(payload_environment(&command, true, inherited()).is_empty());
        command.env_remove("LD_BIND_NOW");
        let removed = payload_environment(&command, false, inherited());
        assert_eq!(removed.last(), Some(&(b"LD_BIND_NOW".to_vec(), None)));
        let mut guardian = Command::new("/bin/true");
        strip_sensitive_parent_env(&mut guardian, inherited());
        assert_eq!(
            guardian.get_envs().collect::<Vec<_>>(),
            vec![(std::ffi::OsStr::new("LD_BIND_NOW"), None)]
        );
    }

    use std::collections::BTreeMap;

    use super::*;
    use crate::process::{EnvMode, OutputCapture, OwnerDeathPolicy};

    #[test]
    fn guardian_request_keeps_explicit_credentials_out_of_argv_and_env() {
        // What this asserts is that an explicit credential travels over the
        // private pipe rather than through argv or the environment. It
        // inherits otherwise, and since harn#8477 an inheriting spawn states
        // that rather than getting it from the absence of a policy.
        let _environment = crate::process::test_support::declare_inherited();
        let canary = "guardian-request-pipe-canary";
        let spec = SpawnSpec {
            builtin: "guardian_request_test",
            program: "/usr/bin/env".to_string(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::from([("EXAMPLE_API_KEY".to_string(), canary.to_string())]),
            env_remove: Vec::new(),
            env_mode: EnvMode::Patch,
            use_stdin: false,
            configure_process_group: true,
            owner_death: OwnerDeathPolicy::KillContainment,
            output_capture: OutputCapture::Pipe,
        };

        let cleanup_token = harn_vm::op_interrupt::new_process_cleanup_token();
        let PreparedGuardian {
            command: guardian,
            request,
            ..
        } = prepare_guardian(&spec, cleanup_token.clone()).expect("prepare guardian");
        harn_vm::op_interrupt::remove_process_owner_group_journal(&cleanup_token);
        let decoded: PreparedCommand =
            serde_json::from_slice(&request).expect("decode private guardian request");
        assert!(
            decoded.env.iter().any(|(key, value)| {
                key.as_slice() == b"EXAMPLE_API_KEY" && value.as_deref() == Some(canary.as_bytes())
            }),
            "the private request must still carry the explicit child credential"
        );
        assert!(
            guardian
                .get_args()
                .all(|arg| !arg.to_string_lossy().contains(canary)),
            "the guardian argv must not carry child credentials"
        );
        assert!(
            guardian.get_envs().all(|(key, value)| {
                !key.to_string_lossy().contains(canary)
                    && !value.is_some_and(|value| value.to_string_lossy().contains(canary))
            }),
            "the guardian environment must not carry child credentials"
        );
    }

    #[test]
    fn guardian_scrubs_sensitive_values_inherited_from_its_parent() {
        let mut guardian = Command::new("/usr/bin/true");
        strip_sensitive_parent_env(
            &mut guardian,
            [
                (
                    OsString::from("EXAMPLE_API_KEY"),
                    OsString::from("secret-canary"),
                ),
                (OsString::from("PATH"), OsString::from("/usr/bin")),
                (OsString::from("LD_BIND_NOW"), OsString::from("1")),
            ],
        );

        let env = guardian.get_envs().collect::<Vec<_>>();
        assert!(env.iter().any(|(key, value)| *key == OsStr::new("LD_BIND_NOW") && value.is_none()),
            "trusted guardian setup removes parent loader controls while the payload environment remains in its pipe request");
        assert!(
            env.iter()
                .any(|(key, value)| { *key == OsStr::new("EXAMPLE_API_KEY") && value.is_none() }),
            "the guardian must remove inherited credentials before re-exec"
        );
        assert!(
            env.iter().all(|(key, _)| *key != OsStr::new("PATH")),
            "the guardian must preserve ordinary inherited environment"
        );
    }
}

/// Unix alone uses the re-exec guardian; Windows uses a Job Object.
#[cfg(not(unix))]
pub fn run_if_requested() -> bool {
    false
}

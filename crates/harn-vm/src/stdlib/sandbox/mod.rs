//! Process sandbox dispatch and per-platform OS confinement.
//!
//! The runtime exposes one stable surface — [`command_output`],
//! [`std_command_for`], [`tokio_command_for`], plus the
//! `enforce_*` helpers — and dispatches into a per-OS
//! [`SandboxBackend`] selected at compile time. The backend chooses
//! how to attach the active capability ceiling to the spawn:
//!
//! * **Linux** ([`linux::Backend`]): Landlock LSM filesystem scoping
//!   plus a default-deny seccomp-bpf syscall blocklist installed via
//!   `pre_exec`, gated behind `PR_SET_NO_NEW_PRIVS`.
//! * **macOS** ([`macos::Backend`]): a `sandbox-exec` profile rendered
//!   from the active capability set wraps the spawn.
//! * **Windows** ([`windows::Backend`]): low-integrity AppContainer +
//!   restricted token + Job Object launched directly through
//!   `CreateProcessW`.
//! * **OpenBSD** ([`openbsd::Backend`]): pledge/unveil applied via
//!   `pre_exec` on top of the standard `Command` plumbing.
//!
//! The [`SandboxProfile`] selected by the active [`CapabilityPolicy`]
//! controls how strictly the backend is required:
//!
//! * `Unrestricted` — bypass everything (path enforcement and OS
//!   confinement).
//! * `Worktree` — workspace path enforcement; OS confinement is
//!   best-effort (warn-and-skip when unavailable). Honors
//!   `HARN_HANDLER_SANDBOX={off,warn,enforce}`.
//! * `OsHardened` — workspace path enforcement; OS confinement is
//!   required. Spawns fail with `tool_rejected` if the platform
//!   mechanism is unavailable, regardless of `HARN_HANDLER_SANDBOX`.
//! * `Wasi` — testbench mode; subprocesses are intercepted by the
//!   process tape and resolved against recorded WASI modules.
//!
//! Per-platform capability → kernel-knob mappings are documented in
//! `docs/src/sandboxing.md`.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[cfg(target_os = "macos")]
use crate::orchestration::ProcessSandboxPreset;
use crate::orchestration::{CapabilityPolicy, SandboxProfile};
use crate::value::{ErrorCategory, VmError, VmValue};
use crate::vm::Vm;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "openbsd")]
mod openbsd;
#[cfg(target_os = "windows")]
mod windows;

const HANDLER_SANDBOX_ENV: &str = "HARN_HANDLER_SANDBOX";

thread_local! {
    static WARNED_KEYS: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
}

/// The kind of filesystem access a path-scope check is guarding. This drives
/// the verb rendered in rejection messages and the narrow standard-device
/// exception; ordinary files are otherwise scoped by the same workspace roots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsAccess {
    Read,
    Write,
    Delete,
}

#[derive(Clone, Debug, Default)]
pub struct ProcessCommandConfig {
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub stdin_null: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SandboxFallback {
    Off,
    Warn,
    Enforce,
}

/// Trait implemented once per supported host OS. Each backend knows
/// how to attach the active capability ceiling to a `Command` /
/// `tokio::process::Command`, or — on Windows where the standard
/// process types cannot carry an AppContainer — how to drive an
/// equivalent custom spawn that returns an `Output`.
///
/// One concrete implementation is selected at compile time via `cfg`
/// gating in this module. Callers should not reach for the trait
/// directly; the module-level `command_output` / `std_command_for` /
/// `tokio_command_for` entry points dispatch through it.
pub(crate) trait SandboxBackend {
    /// Stable identifier used in diagnostics and conformance fixtures.
    fn name() -> &'static str;

    /// Whether the platform mechanism this backend uses is available
    /// on the running host (e.g. Landlock kernel support, the
    /// `/usr/bin/sandbox-exec` binary, AppContainer APIs).
    fn available() -> bool;

    /// Apply the per-spawn confinement to a [`std::process::Command`].
    /// Returns `Ok(())` if the backend can attach inline (Linux
    /// `pre_exec`, OpenBSD pledge/unveil), or
    /// [`PrepareOutcome::WrappedExec`] when the spawn must be
    /// re-routed through a wrapper binary (macOS `sandbox-exec`).
    fn prepare_std_command(
        program: &str,
        args: &[String],
        command: &mut Command,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError>;

    /// Same as [`prepare_std_command`], but for `tokio::process::Command`.
    fn prepare_tokio_command(
        program: &str,
        args: &[String],
        command: &mut tokio::process::Command,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError>;

    /// Direct spawn that returns the captured `Output`. Windows uses
    /// this because AppContainer cannot be attached to a vanilla
    /// `Command`; other platforms can fall back to the default
    /// implementation that builds a `Command` and runs it.
    fn run_to_output(
        program: &str,
        args: &[String],
        config: &ProcessCommandConfig,
        policy: &CapabilityPolicy,
        profile: SandboxProfile,
    ) -> Result<Output, VmError> {
        let mut command = build_std_command::<Self>(program, args, policy, profile)?;
        apply_process_config(&mut command, config);
        command
            .output()
            .map_err(|error| process_spawn_error(&error).unwrap_or_else(|| spawn_error(error)))
    }
}

/// What [`SandboxBackend::prepare_std_command`] / `_tokio_command`
/// produced: either the original spawn target with sandboxing applied
/// inline, or a wrapper binary that should be invoked instead.
pub(crate) enum PrepareOutcome {
    /// Use the prepared command unchanged.
    Direct,
    /// Replace the spawn target with the wrapper binary and args
    /// (e.g. `sandbox-exec -p '<profile>' -- <program> <args...>`).
    /// Only macOS produces this today; on other platforms the variant
    /// stays defined so the trait surface is portable, but the
    /// build-time dead-code lint would otherwise flip.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    WrappedExec { wrapper: String, args: Vec<String> },
}

#[cfg(target_os = "linux")]
type ActiveBackend = linux::Backend;
#[cfg(target_os = "macos")]
type ActiveBackend = macos::Backend;
#[cfg(target_os = "openbsd")]
type ActiveBackend = openbsd::Backend;
#[cfg(target_os = "windows")]
type ActiveBackend = windows::Backend;
#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
)))]
type ActiveBackend = NoopBackend;

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
)))]
pub(crate) struct NoopBackend;

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
)))]
impl SandboxBackend for NoopBackend {
    fn name() -> &'static str {
        "noop"
    }
    fn available() -> bool {
        false
    }
    fn prepare_std_command(
        _program: &str,
        _args: &[String],
        _command: &mut Command,
        _policy: &CapabilityPolicy,
        _profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        Ok(PrepareOutcome::Direct)
    }
    fn prepare_tokio_command(
        _program: &str,
        _args: &[String],
        _command: &mut tokio::process::Command,
        _policy: &CapabilityPolicy,
        _profile: SandboxProfile,
    ) -> Result<PrepareOutcome, VmError> {
        Ok(PrepareOutcome::Direct)
    }
}

pub(crate) fn reset_sandbox_state() {
    WARNED_KEYS.with(|keys| keys.borrow_mut().clear());
}

/// Stable identifier for the platform sandbox backend selected at
/// compile time. Surfaced for diagnostics and conformance fixtures so
/// callers can record which backend produced a recorded run.
pub fn active_backend_name() -> &'static str {
    ActiveBackend::name()
}

/// Whether the platform mechanism backing the active sandbox backend
/// is available on the running host. Used by conformance fixtures and
/// the `harn doctor` flow to skip OS-hardened checks on hosts without
/// the required kernel support.
pub fn active_backend_available() -> bool {
    ActiveBackend::available()
}

/// Register Harn-callable introspection builtins for the sandbox.
/// Intended for diagnostics, `harn doctor`, and conformance fixtures —
/// not as a way to mutate runtime sandbox behavior from a script.
pub fn register_sandbox_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&crate::stdlib::macros::VmBuiltinDef] = &[
    &SANDBOX_ACTIVE_BACKEND_IMPL_DEF,
    &SANDBOX_BACKEND_AVAILABLE_IMPL_DEF,
    &SANDBOX_ACTIVE_PROFILE_IMPL_DEF,
];

#[crate::stdlib::macros::harn_builtin(
    sig = "sandbox_active_backend() -> string",
    category = "sandbox"
)]
fn sandbox_active_backend_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::String(std::sync::Arc::from(active_backend_name())))
}

#[crate::stdlib::macros::harn_builtin(
    sig = "sandbox_backend_available() -> bool",
    category = "sandbox"
)]
fn sandbox_backend_available_impl(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(active_backend_available()))
}

#[crate::stdlib::macros::harn_builtin(
    sig = "sandbox_active_profile() -> string",
    category = "sandbox"
)]
fn sandbox_active_profile_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let profile = crate::orchestration::current_execution_policy()
        .map(|policy| policy.sandbox_profile)
        .unwrap_or(SandboxProfile::Unrestricted);
    Ok(VmValue::String(std::sync::Arc::from(profile.as_str())))
}

/// A workspace-root scope violation: a path that resolved outside every
/// configured workspace root under a restricted [`SandboxProfile`].
///
/// This is the `VmError`-free shape returned by [`check_fs_path_scope`] so
/// that crates outside `harn-vm` (today: `harn-hostlib`) can enforce the
/// same scope policy and render the violation onto their own error type.
#[derive(Clone, Debug)]
pub struct SandboxViolation {
    /// The path the call attempted to touch, normalized against the
    /// active policy (CWD-relative paths resolved to absolute, `..`
    /// collapsed, symlinks canonicalized where the path exists).
    pub attempted: PathBuf,
    /// The writable workspace roots the path was checked against,
    /// normalized the same way as `attempted`.
    pub roots: Vec<PathBuf>,
    /// Whether the rejected access was a read, write, or delete.
    pub access: FsAccess,
    /// True when the path resolved *inside* a read-only root: it is in
    /// scope for reads, and only the attempted mutation is denied. False
    /// when the path fell outside every configured root entirely.
    pub read_only: bool,
}

impl SandboxViolation {
    /// Render the canonical rejection message. Matches the text produced
    /// by [`enforce_fs_path`] so the `harness.fs.*` and hostlib surfaces
    /// reject an out-of-root path identically.
    pub fn message(&self, builtin: &str) -> String {
        if self.read_only {
            return format!(
                "sandbox violation: builtin '{builtin}' attempted to {} '{}' under a read-only workspace root",
                self.access.verb(),
                self.attempted.display(),
            );
        }
        format!(
            "sandbox violation: builtin '{builtin}' attempted to {} '{}' outside workspace_roots [{}]",
            self.access.verb(),
            self.attempted.display(),
            self.roots
                .iter()
                .map(|root| root.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// Check whether `path` is inside the active policy's workspace roots.
///
/// Returns `Ok(())` when no execution policy is active, when the active
/// profile is [`SandboxProfile::Unrestricted`], when the normalized path
/// falls within a writable workspace root, or — for [`FsAccess::Read`]
/// only — when it falls within a read-only root. A write/delete that
/// resolves under a read-only root is rejected with `read_only` set, as
/// is any access that falls outside every configured root.
///
/// This is the public, `VmError`-free entry point embedders use to apply
/// workspace-root scoping to their own host calls. The in-crate
/// `harness.fs.*` builtins funnel through [`enforce_fs_path`], which wraps
/// this with a `VmError`; both share the same path normalization and
/// rejection text.
pub fn check_fs_path_scope(path: &Path, access: FsAccess) -> Result<(), SandboxViolation> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Ok(());
    };
    if matches!(policy.sandbox_profile, SandboxProfile::Unrestricted) {
        return Ok(());
    }
    // Standard process I/O device files are not workspace filesystem
    // mutations: writing to /dev/stdout, /dev/stderr, or /dev/null (and the
    // numeric /dev/fd/<N> descriptors they alias) targets the process's own
    // output streams, not the sandboxed tree. A pipeline that falls back to
    // /dev/stdout for debug output must not read as a sandbox violation, so
    // allow these regardless of the configured roots. Matched on the
    // lexically-normalized path (not the canonicalized form): canonicalize()
    // rewrites /dev/stdout to a per-process /dev/fd/<…>.output alias that no
    // longer looks like a standard device. Kept deliberately narrow — only
    // the well-known device files, no broader /dev access.
    if is_standard_io_device_for_access(&normalize_io_device_path(path), access) {
        return Ok(());
    }
    let candidate = normalize_for_policy(path);
    let roots = normalized_workspace_roots(&policy);
    if roots.iter().any(|root| path_is_within(&candidate, root)) {
        return Ok(());
    }
    let read_only_roots = normalized_read_only_roots(&policy);
    let within_read_only = read_only_roots
        .iter()
        .any(|root| path_is_within(&candidate, root));
    if within_read_only && access == FsAccess::Read {
        return Ok(());
    }
    Err(SandboxViolation {
        attempted: candidate,
        roots,
        access,
        read_only: within_read_only,
    })
}

pub(crate) fn enforce_fs_path(builtin: &str, path: &Path, access: FsAccess) -> Result<(), VmError> {
    check_fs_path_scope(path, access)
        .map_err(|violation| sandbox_rejection(violation.message(builtin)))
}

pub fn enforce_process_cwd(path: &Path) -> Result<(), VmError> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Ok(());
    };
    if matches!(policy.sandbox_profile, SandboxProfile::Unrestricted) {
        return Ok(());
    }
    let candidate = normalize_for_policy(path);
    let roots = normalized_workspace_roots(&policy);
    if roots.iter().any(|root| path_is_within(&candidate, root)) {
        return Ok(());
    }
    Err(sandbox_rejection(format!(
        "sandbox violation: process cwd '{}' is outside workspace_roots [{}]",
        candidate.display(),
        roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

pub fn std_command_for(program: &str, args: &[String]) -> Result<Command, VmError> {
    let (policy, profile) = match active_sandbox_policy() {
        Some(value) => value,
        None => {
            let mut command = Command::new(program);
            command.args(args);
            return Ok(command);
        }
    };
    build_std_command::<ActiveBackend>(program, args, &policy, profile)
}

pub fn tokio_command_for(
    program: &str,
    args: &[String],
) -> Result<tokio::process::Command, VmError> {
    let (policy, profile) = match active_sandbox_policy() {
        Some(value) => value,
        None => {
            let mut command = tokio::process::Command::new(program);
            command.args(args);
            return Ok(command);
        }
    };
    build_tokio_command::<ActiveBackend>(program, args, &policy, profile)
}

pub fn command_output(
    program: &str,
    args: &[String],
    config: &ProcessCommandConfig,
) -> Result<Output, VmError> {
    // Testbench replay mode short-circuits the spawn entirely.
    // Recording mode falls through; the duration is captured by the
    // recording handle below using the injected mock clock when one
    // is active.
    if let Some(intercepted) =
        crate::testbench::process_tape::intercept_spawn(program, args, config.cwd.as_deref())
    {
        return intercepted.map_err(|message| {
            VmError::Thrown(crate::value::VmValue::String(std::sync::Arc::from(message)))
        });
    }

    let recording =
        crate::testbench::process_tape::start_recording(program, args, config.cwd.as_deref());

    let output = match active_sandbox_policy() {
        Some((policy, profile)) => {
            ActiveBackend::run_to_output(program, args, config, &policy, profile)?
        }
        None => {
            let mut command = Command::new(program);
            command.args(args);
            apply_process_config(&mut command, config);
            command.output().map_err(|error| {
                process_spawn_error(&error).unwrap_or_else(|| spawn_error(error))
            })?
        }
    };
    if let Some(error) = process_violation_error(&output) {
        return Err(error);
    }
    if let Some(span) = recording {
        span.finish(&output);
    }
    Ok(output)
}

fn build_std_command<B: SandboxBackend + ?Sized>(
    program: &str,
    args: &[String],
    policy: &CapabilityPolicy,
    profile: SandboxProfile,
) -> Result<Command, VmError> {
    let mut command = Command::new(program);
    command.args(args);
    match B::prepare_std_command(program, args, &mut command, policy, profile)? {
        PrepareOutcome::Direct => Ok(command),
        PrepareOutcome::WrappedExec { wrapper, args } => {
            let mut wrapped = Command::new(wrapper);
            wrapped.args(args);
            Ok(wrapped)
        }
    }
}

fn build_tokio_command<B: SandboxBackend + ?Sized>(
    program: &str,
    args: &[String],
    policy: &CapabilityPolicy,
    profile: SandboxProfile,
) -> Result<tokio::process::Command, VmError> {
    let mut command = tokio::process::Command::new(program);
    command.args(args);
    match B::prepare_tokio_command(program, args, &mut command, policy, profile)? {
        PrepareOutcome::Direct => Ok(command),
        PrepareOutcome::WrappedExec { wrapper, args } => {
            let mut wrapped = tokio::process::Command::new(wrapper);
            wrapped.args(args);
            Ok(wrapped)
        }
    }
}

pub fn process_violation_error(output: &std::process::Output) -> Option<VmError> {
    let policy = crate::orchestration::current_execution_policy()?;
    if matches!(policy.sandbox_profile, SandboxProfile::Unrestricted) {
        return None;
    }
    if effective_fallback(policy.sandbox_profile) == SandboxFallback::Off
        || !ActiveBackend::available()
    {
        return None;
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    if !output.status.success()
        && (stderr.contains("operation not permitted")
            || stderr.contains("permission denied")
            || stderr.contains("access is denied")
            || stdout.contains("operation not permitted"))
    {
        return Some(sandbox_rejection(format!(
            "sandbox violation: process was denied by the OS sandbox (status {})",
            output.status.code().unwrap_or(-1)
        )));
    }
    if sandbox_signal_status(output) {
        return Some(sandbox_rejection(format!(
            "sandbox violation: process was terminated by the OS sandbox (status {})",
            output.status
        )));
    }
    None
}

pub fn process_spawn_error(error: &std::io::Error) -> Option<VmError> {
    let policy = crate::orchestration::current_execution_policy()?;
    if matches!(policy.sandbox_profile, SandboxProfile::Unrestricted) {
        return None;
    }
    if effective_fallback(policy.sandbox_profile) == SandboxFallback::Off
        || !ActiveBackend::available()
    {
        return None;
    }
    let message = error.to_string().to_ascii_lowercase();
    if error.kind() == std::io::ErrorKind::PermissionDenied
        || message.contains("operation not permitted")
        || message.contains("permission denied")
        || message.contains("access is denied")
    {
        return Some(sandbox_rejection(format!(
            "sandbox violation: process was denied by the OS sandbox before exec: {error}"
        )));
    }
    None
}

#[cfg(unix)]
fn sandbox_signal_status(output: &std::process::Output) -> bool {
    use std::os::unix::process::ExitStatusExt;

    matches!(
        output.status.signal(),
        Some(libc::SIGSYS) | Some(libc::SIGABRT) | Some(libc::SIGKILL)
    )
}

#[cfg(not(unix))]
fn sandbox_signal_status(_output: &std::process::Output) -> bool {
    false
}

/// Returns the active capability policy and the resolved sandbox
/// profile, or `None` if confinement should be skipped entirely. The
/// `Unrestricted` profile and the `HARN_HANDLER_SANDBOX=off` escape
/// hatch both produce `None`. The `Wasi` profile also produces `None`
/// on the host spawn path — testbench mode intercepts subprocesses
/// before they reach this layer, so the host-spawn fallback should be
/// a normal direct exec.
pub(crate) fn active_sandbox_policy() -> Option<(CapabilityPolicy, SandboxProfile)> {
    let policy = crate::orchestration::current_execution_policy()?;
    let profile = policy.sandbox_profile;
    match profile {
        SandboxProfile::Unrestricted | SandboxProfile::Wasi => None,
        SandboxProfile::Worktree | SandboxProfile::OsHardened => {
            if effective_fallback(profile) == SandboxFallback::Off {
                None
            } else {
                Some((policy, profile))
            }
        }
    }
}

fn apply_process_config(command: &mut Command, config: &ProcessCommandConfig) {
    if let Some(cwd) = config.cwd.as_ref() {
        command.current_dir(cwd);
    }
    command.envs(config.env.iter().map(|(key, value)| (key, value)));
    if config.stdin_null {
        command.stdin(Stdio::null());
    }
}

fn spawn_error(error: std::io::Error) -> VmError {
    VmError::Thrown(crate::value::VmValue::String(std::sync::Arc::from(
        format!("process spawn failed: {error}"),
    )))
}

/// Resolve the fallback policy for the requested profile. `OsHardened`
/// always enforces — that is the entire point of the profile, so the
/// `HARN_HANDLER_SANDBOX` env var cannot weaken it. `Worktree` honors
/// the env var (default `warn`).
pub(crate) fn effective_fallback(profile: SandboxProfile) -> SandboxFallback {
    if matches!(profile, SandboxProfile::OsHardened) {
        return SandboxFallback::Enforce;
    }
    match std::env::var(HANDLER_SANDBOX_ENV)
        .unwrap_or_else(|_| "warn".to_string())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "0" | "false" | "off" | "none" => SandboxFallback::Off,
        "1" | "true" | "enforce" | "required" => SandboxFallback::Enforce,
        _ => SandboxFallback::Warn,
    }
}

pub(crate) fn warn_once(key: &str, message: &str) {
    let inserted = WARNED_KEYS.with(|keys| keys.borrow_mut().insert(key.to_string()));
    if inserted {
        crate::events::log_warn("handler_sandbox", message);
    }
}

pub(crate) fn sandbox_rejection(message: String) -> VmError {
    VmError::CategorizedError {
        message,
        category: ErrorCategory::ToolRejected,
    }
}

/// Helper for backends that can't attach confinement at all (macOS
/// without `/usr/bin/sandbox-exec`, Windows when called through the
/// `Command`-returning entry points): either fail loudly under
/// `OsHardened` / `enforce`, or warn once and proceed direct.
///
/// Linux and OpenBSD don't reach this path — they install confinement
/// in `pre_exec` and surface unavailability through `landlock_profile`
/// directly. The dead-code lint allow keeps the helper compilable on
/// targets where no backend uses it.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub(crate) fn unavailable(
    message: &str,
    profile: SandboxProfile,
) -> Result<PrepareOutcome, VmError> {
    match effective_fallback(profile) {
        SandboxFallback::Off | SandboxFallback::Warn => {
            warn_once("handler_sandbox_unavailable", message);
            Ok(PrepareOutcome::Direct)
        }
        SandboxFallback::Enforce => Err(sandbox_rejection(format!(
            "{message}; set {HANDLER_SANDBOX_ENV}=warn or off to run unsandboxed"
        ))),
    }
}

fn normalized_workspace_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    if policy.workspace_roots.is_empty() {
        return vec![normalize_for_policy(
            &crate::stdlib::process::execution_root_path(),
        )];
    }
    policy
        .workspace_roots
        .iter()
        .map(|root| normalize_for_policy(&resolve_policy_path(root)))
        .collect()
}

pub(crate) fn process_sandbox_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    normalized_workspace_roots(policy)
}

/// Normalize the policy's read-only roots. Unlike
/// [`normalized_workspace_roots`], an empty list stays empty — read-only
/// scope is purely additive, so there is no execution-root fallback to
/// synthesize.
fn normalized_read_only_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    policy
        .read_only_roots
        .iter()
        .map(|root| normalize_for_policy(&resolve_policy_path(root)))
        .collect()
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
pub(crate) fn process_sandbox_readonly_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    normalized_read_only_roots(policy)
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
pub(crate) fn process_sandbox_policy_read_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    normalized_process_roots(&policy.process_sandbox.read_roots)
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
pub(crate) fn process_sandbox_policy_write_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    normalized_process_roots(&policy.process_sandbox.write_roots)
}

#[cfg(target_os = "macos")]
pub(crate) fn process_sandbox_presets(policy: &CapabilityPolicy) -> Vec<ProcessSandboxPreset> {
    policy.process_sandbox.effective_presets()
}

/// Home-relative install locations for common language toolchains.
///
/// WHY: the process sandbox confines filesystem reads to the workspace
/// roots, but most developers install their language runtimes under
/// `$HOME` (uv-managed CPython, rustup toolchains, nvm/fnm/volta Node,
/// SDKMAN JDKs, a user `GOPATH`). When an agent runs `uv run pytest`,
/// `cargo test`, or `npm test`, the interpreter/linker then tries to
/// open shared libraries and toolchain binaries that live outside the
/// workspace and the kernel blocks the open — e.g.:
///
///   dyld: Library not loaded: @rpath/libpython3.13.dylib
///     Reason: '~/.local/share/uv/python/.../libpython3.13.dylib'
///       (file system sandbox blocked open())
///
/// Granting these directories **read + execute** (never write) lets
/// home-installed toolchains load while keeping the sandbox otherwise
/// tight.
///
/// SECURITY: every entry is an execution-relevant *runtime* subpath, not
/// a tool's whole config root. We deliberately scope to install dirs
/// (`~/.rustup/toolchains`, `$CARGO_HOME/bin`) rather than the parents
/// (`~/.cargo`, which holds `credentials.toml`; `~/.gradle/caches`, which
/// is large and may cache credentials). Access is read-only on every
/// backend, so even a path that incidentally contains a secret cannot be
/// modified or exfiltrated-by-write through these grants. Each candidate
/// is resolved from its canonical environment variable first, then a
/// `$HOME`-relative fallback, and is only emitted when the directory
/// actually exists — so the profile never bloats with phantom paths.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
pub(crate) fn toolchain_read_roots() -> Vec<PathBuf> {
    toolchain_read_roots_from(
        |key| std::env::var_os(key).map(PathBuf::from),
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from),
        |path| path.is_dir(),
    )
}

/// Pure core of [`toolchain_read_roots`], parameterized over environment
/// and filesystem lookups so it can be unit-tested with a temp `HOME` and
/// synthetic env vars. `env` resolves an environment variable to a path,
/// `home` is the resolved home directory (if any), and `exists` reports
/// whether a candidate directory is present.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
fn toolchain_read_roots_from(
    env: impl Fn(&str) -> Option<PathBuf>,
    home: Option<PathBuf>,
    exists: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    // Keep every entry execution-relevant and free of credential-bearing
    // config files (see the doc comment above).
    //
    // Each row is `(env_var, home_fallback, narrow_suffix)`:
    //   * `env_var`        — canonical env var naming the *root* dir.
    //   * `home_fallback`  — `$HOME`-relative root used when the env var is
    //                        unset, or `None` for env-only roots (GOROOT).
    //   * `narrow_suffix`  — appended to the resolved root to reach the
    //                        execution-relevant, credential-free leaf, or
    //                        `""` to use the root as-is.
    //
    // Narrowing applies whether the root came from the env var or the home
    // fallback, so a custom `CARGO_HOME` still scopes to its `bin/` only.
    let candidates: &[(&str, Option<&str>, &str)] = &[
        // Python / uv-managed CPython; the env var already points at the
        // python install tree, so no suffix.
        ("UV_PYTHON_INSTALL_DIR", Some(".local/share/uv/python"), ""),
        // pyenv version installs (skip shims/config at the root).
        ("PYENV_ROOT", Some(".pyenv"), "versions"),
        // Rust: the whole rustup home (toolchains + settings.toml — rustup
        // reads settings.toml FIRST to resolve the default toolchain, and it
        // holds no credentials), plus the cargo bin/registry/git caches that
        // `cargo build` reads — but NOT all of ~/.cargo, which holds
        // credentials.toml (and config.toml), so those stay denied.
        ("RUSTUP_HOME", Some(".rustup"), ""),
        ("CARGO_HOME", Some(".cargo"), "bin"),
        ("CARGO_HOME", Some(".cargo"), "registry"),
        ("CARGO_HOME", Some(".cargo"), "git"),
        // Node version managers.
        ("NVM_DIR", Some(".nvm"), "versions"),
        ("VOLTA_HOME", Some(".volta"), ""),
        ("FNM_DIR", Some(".fnm"), ""),
        // Go: GOROOT (toolchain tree, env-only) + GOPATH/bin (go install
        // binaries). GOPATH defaults to ~/go.
        ("GOROOT", None, ""),
        ("GOPATH", Some("go"), "bin"),
        // JVM toolchains via SDKMAN candidate installs (skip the broad
        // ~/.gradle/caches, which is large and may cache credentials).
        ("SDKMAN_DIR", Some(".sdkman"), "candidates"),
    ];

    let narrow = |root: PathBuf, suffix: &str| -> PathBuf {
        if suffix.is_empty() {
            root
        } else {
            root.join(suffix)
        }
    };

    let mut roots: Vec<PathBuf> = Vec::new();
    for (key, home_fallback, suffix) in candidates {
        let root = match env(key) {
            Some(path) => Some(path),
            None => home_fallback.and_then(|fallback| home.as_ref().map(|h| h.join(fallback))),
        };
        if let Some(root) = root {
            let candidate = narrow(root, suffix);
            if exists(&candidate) && !roots.contains(&candidate) {
                roots.push(candidate);
            }
        }
    }
    roots
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
fn normalized_process_roots(roots: &[String]) -> Vec<PathBuf> {
    roots
        .iter()
        .map(|root| normalize_for_policy(&resolve_policy_path(root)))
        .collect()
}

fn resolve_policy_path(path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        crate::stdlib::process::execution_root_path().join(candidate)
    }
}

fn normalize_for_policy(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        crate::stdlib::process::execution_root_path().join(path)
    };
    let absolute = normalize_lexically(&absolute);
    if let Ok(canonical) = absolute.canonicalize() {
        return canonical;
    }

    let mut existing = absolute.as_path();
    let mut suffix = Vec::new();
    while !existing.exists() {
        let Some(parent) = existing.parent() else {
            return normalize_lexically(&absolute);
        };
        if let Some(name) = existing.file_name() {
            suffix.push(name.to_os_string());
        }
        existing = parent;
    }

    let mut normalized = existing
        .canonicalize()
        .unwrap_or_else(|_| normalize_lexically(existing));
    for component in suffix.iter().rev() {
        normalized.push(component);
    }
    normalize_lexically(&normalized)
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

/// Resolve `path` to an absolute, lexically-normalized form for the standard
/// I/O device check. Unlike [`normalize_for_policy`] this never calls
/// `canonicalize`, which on macOS rewrites `/dev/stdout` to a per-process
/// `/dev/fd/<…>.output` alias that no longer matches a known device file.
fn normalize_io_device_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        crate::stdlib::process::execution_root_path().join(path)
    };
    normalize_lexically(&absolute)
}

/// Whether `path` is one of the standard process I/O device files that the
/// sandbox treats as a stream rather than a workspace mutation for this access:
/// stdin is read-only, stdout/stderr/null are read/write, and delete is never a
/// stream operation. `path` must already be absolute and lexically normalized.
fn is_standard_io_device_for_access(path: &Path, access: FsAccess) -> bool {
    match access {
        FsAccess::Read => {
            matches!(
                path.to_str(),
                Some("/dev/stdin" | "/dev/stdout" | "/dev/stderr" | "/dev/null")
            ) || is_dev_fd_descriptor(path)
        }
        FsAccess::Write => {
            matches!(
                path.to_str(),
                Some("/dev/stdout" | "/dev/stderr" | "/dev/null")
            ) || is_dev_fd_descriptor(path)
        }
        FsAccess::Delete => false,
    }
}

/// Whether `path` is exactly `/dev/fd/<N>` for a non-empty run of ASCII
/// digits (the numeric file-descriptor aliases for the standard streams).
fn is_dev_fd_descriptor(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    let Some(fd) = text.strip_prefix("/dev/fd/") else {
        return false;
    };
    !fd.is_empty() && fd.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "openbsd"))]
pub(crate) fn policy_allows_network(policy: &CapabilityPolicy) -> bool {
    fn rank(value: &str) -> usize {
        match value {
            "none" => 0,
            "read_only" => 1,
            "workspace_write" => 2,
            "process_exec" => 3,
            "network" => 4,
            _ => 5,
        }
    }
    policy
        .side_effect_level
        .as_ref()
        .map(|level| rank(level) >= rank("network"))
        .unwrap_or(true)
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
pub(crate) fn policy_allows_workspace_write(policy: &CapabilityPolicy) -> bool {
    policy.capabilities.is_empty()
        || policy_allows_capability(policy, "workspace", &["write_text", "delete"])
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
pub(crate) fn policy_allows_capability(
    policy: &CapabilityPolicy,
    capability: &str,
    ops: &[&str],
) -> bool {
    policy
        .capabilities
        .get(capability)
        .map(|allowed| {
            ops.iter()
                .any(|op| allowed.iter().any(|candidate| candidate == op))
        })
        .unwrap_or(false)
}

impl FsAccess {
    fn verb(self) -> &'static str {
        match self {
            FsAccess::Read => "read",
            FsAccess::Write => "write",
            FsAccess::Delete => "delete",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{pop_execution_policy, push_execution_policy};

    #[test]
    fn missing_create_path_normalizes_against_existing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/../new.txt");
        let normalized = normalize_for_policy(&nested);
        assert_eq!(
            normalized,
            normalize_for_policy(&dir.path().join("new.txt"))
        );
    }

    #[test]
    fn empty_workspace_roots_default_to_execution_root_for_fs_paths() {
        let dir = tempfile::tempdir().unwrap();
        crate::stdlib::process::set_thread_execution_context(Some(
            crate::orchestration::RunExecutionRecord {
                cwd: Some(dir.path().to_string_lossy().into_owned()),
                source_dir: None,
                env: Default::default(),
                adapter: None,
                repo_path: None,
                worktree_path: None,
                branch: None,
                base_ref: None,
                cleanup: None,
            },
        ));
        push_execution_policy(CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            ..CapabilityPolicy::default()
        });

        assert!(
            enforce_fs_path("read_file", &dir.path().join("inside.txt"), FsAccess::Read).is_ok()
        );
        let outside = tempfile::tempdir().unwrap();
        assert!(enforce_fs_path(
            "read_file",
            &outside.path().join("outside.txt"),
            FsAccess::Read
        )
        .is_err());

        pop_execution_policy();
        crate::stdlib::process::set_thread_execution_context(None);
    }

    #[test]
    fn empty_workspace_roots_default_to_execution_root_for_process_cwd() {
        let dir = tempfile::tempdir().unwrap();
        crate::stdlib::process::set_thread_execution_context(Some(
            crate::orchestration::RunExecutionRecord {
                cwd: Some(dir.path().to_string_lossy().into_owned()),
                source_dir: None,
                env: Default::default(),
                adapter: None,
                repo_path: None,
                worktree_path: None,
                branch: None,
                base_ref: None,
                cleanup: None,
            },
        ));
        push_execution_policy(CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            ..CapabilityPolicy::default()
        });

        assert!(enforce_process_cwd(dir.path()).is_ok());
        let outside = tempfile::tempdir().unwrap();
        assert!(enforce_process_cwd(outside.path()).is_err());

        pop_execution_policy();
        crate::stdlib::process::set_thread_execution_context(None);
    }

    #[test]
    fn read_only_root_outside_workspace_allows_read_denies_write() {
        // Models an embedder (burin's in-process TUI) that grants a
        // read-only root R holding bundled pipelines/partials outside the
        // user's writable workspace. A read under R passes; a write under R
        // is denied; a read outside both R and the workspace is denied.
        let workspace = tempfile::tempdir().unwrap();
        let read_only = tempfile::tempdir().unwrap();
        push_execution_policy(CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
            read_only_roots: vec![read_only.path().to_string_lossy().into_owned()],
            ..CapabilityPolicy::default()
        });

        let asset = read_only
            .path()
            .join("partials/agent-web-tools.harn.prompt");
        // READ under the read-only root is allowed.
        assert!(
            check_fs_path_scope(&asset, FsAccess::Read).is_ok(),
            "read under a configured read-only root must be allowed"
        );

        // WRITE under the read-only root is denied, flagged read_only.
        let write_err = check_fs_path_scope(&asset, FsAccess::Write)
            .expect_err("write under a read-only root must be denied");
        assert!(write_err.read_only, "write rejection must set read_only");

        // DELETE under the read-only root is likewise denied.
        assert!(
            check_fs_path_scope(&asset, FsAccess::Delete).is_err(),
            "delete under a read-only root must be denied"
        );

        // A read inside the writable workspace still passes.
        assert!(check_fs_path_scope(&workspace.path().join("src/main.rs"), FsAccess::Read).is_ok());

        // A read outside BOTH the workspace and the read-only root is denied
        // and is NOT flagged read_only (it fell outside every root).
        let stranger = tempfile::tempdir().unwrap();
        let outside_err = check_fs_path_scope(&stranger.path().join("secret.txt"), FsAccess::Read)
            .expect_err("read outside all roots must be denied");
        assert!(
            !outside_err.read_only,
            "out-of-scope rejection must not be flagged read_only"
        );

        pop_execution_policy();
    }

    #[cfg(unix)]
    #[test]
    fn standard_io_device_files_allowed_under_restricted_profile() {
        // Writing to the standard process I/O streams is not a workspace
        // mutation, so a restricted profile with a workspace root that does
        // not contain /dev must still allow them — while a genuine
        // out-of-root write is still rejected.
        let workspace = tempfile::tempdir().unwrap();
        push_execution_policy(CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
            ..CapabilityPolicy::default()
        });

        for device in ["/dev/stdout", "/dev/stderr", "/dev/null"] {
            assert!(
                check_fs_path_scope(Path::new(device), FsAccess::Write).is_ok(),
                "write to standard device {device} must be allowed"
            );
            // Reads of the same devices are likewise allowed.
            assert!(
                check_fs_path_scope(Path::new(device), FsAccess::Read).is_ok(),
                "read of standard device {device} must be allowed"
            );
        }
        assert!(
            check_fs_path_scope(Path::new("/dev/stdin"), FsAccess::Read).is_ok(),
            "read of standard device /dev/stdin must be allowed"
        );
        assert!(
            check_fs_path_scope(Path::new("/dev/stdin"), FsAccess::Write).is_err(),
            "write to /dev/stdin is not a standard output stream"
        );
        assert!(
            check_fs_path_scope(Path::new("/dev/null"), FsAccess::Delete).is_err(),
            "standard devices must not bypass delete scoping"
        );
        // Numeric /dev/fd/<N> descriptors are allowed.
        assert!(check_fs_path_scope(Path::new("/dev/fd/1"), FsAccess::Write).is_ok());
        assert!(check_fs_path_scope(Path::new("/dev/fd/2"), FsAccess::Write).is_ok());

        // A non-device path outside the workspace is still rejected.
        let stranger = tempfile::tempdir().unwrap();
        assert!(
            check_fs_path_scope(&stranger.path().join("escape.txt"), FsAccess::Write).is_err(),
            "a real out-of-root write must still be rejected"
        );
        // Other /dev entries are NOT broadly allowed — the allowlist is narrow.
        assert!(
            check_fs_path_scope(Path::new("/dev/sda"), FsAccess::Write).is_err(),
            "/dev/sda must not be allowed by the standard-device allowlist"
        );
        assert!(
            check_fs_path_scope(Path::new("/dev/fd/notanumber"), FsAccess::Write).is_err(),
            "non-numeric /dev/fd/<x> must not be allowed"
        );

        pop_execution_policy();
    }

    #[test]
    fn is_standard_io_device_matches_only_known_streams() {
        assert!(is_standard_io_device_for_access(
            Path::new("/dev/stdin"),
            FsAccess::Read
        ));
        assert!(!is_standard_io_device_for_access(
            Path::new("/dev/stdin"),
            FsAccess::Write
        ));
        assert!(is_standard_io_device_for_access(
            Path::new("/dev/stdout"),
            FsAccess::Write
        ));
        assert!(is_standard_io_device_for_access(
            Path::new("/dev/stderr"),
            FsAccess::Write
        ));
        assert!(is_standard_io_device_for_access(
            Path::new("/dev/null"),
            FsAccess::Write
        ));
        assert!(is_standard_io_device_for_access(
            Path::new("/dev/fd/0"),
            FsAccess::Read
        ));
        assert!(is_standard_io_device_for_access(
            Path::new("/dev/fd/12"),
            FsAccess::Write
        ));
        assert!(!is_standard_io_device_for_access(
            Path::new("/dev/null"),
            FsAccess::Delete
        ));
        assert!(!is_standard_io_device_for_access(
            Path::new("/dev/fd/"),
            FsAccess::Write
        ));
        assert!(!is_standard_io_device_for_access(
            Path::new("/dev/fd/1a"),
            FsAccess::Write
        ));
        assert!(!is_standard_io_device_for_access(
            Path::new("/dev/stdoutx"),
            FsAccess::Write
        ));
        assert!(!is_standard_io_device_for_access(
            Path::new("/dev/random"),
            FsAccess::Read
        ));
        assert!(!is_standard_io_device_for_access(
            Path::new("/tmp/dev/null"),
            FsAccess::Write
        ));
    }

    #[test]
    fn path_within_root_accepts_root_and_children() {
        let root = Path::new("/tmp/harn-root");
        assert!(path_is_within(root, root));
        assert!(path_is_within(Path::new("/tmp/harn-root/file"), root));
        assert!(!path_is_within(
            Path::new("/tmp/harn-root-other/file"),
            root
        ));
    }

    #[test]
    fn os_hardened_profile_overrides_fallback_env() {
        // `OsHardened` ignores `HARN_HANDLER_SANDBOX=off` — the whole
        // point of the profile is that the OS sandbox is required.
        // We cannot mutate the env here without races, so just check
        // the pure resolution function.
        assert_eq!(
            effective_fallback(SandboxProfile::OsHardened),
            SandboxFallback::Enforce
        );
    }

    #[test]
    fn unrestricted_profile_skips_active_sandbox() {
        let policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Unrestricted,
            workspace_roots: vec!["/tmp".to_string()],
            ..Default::default()
        };
        crate::orchestration::push_execution_policy(policy);
        let result = active_sandbox_policy();
        crate::orchestration::pop_execution_policy();
        assert!(
            result.is_none(),
            "Unrestricted profile must short-circuit sandbox dispatch"
        );
    }

    #[test]
    fn worktree_profile_engages_active_sandbox() {
        let policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec!["/tmp".to_string()],
            ..Default::default()
        };
        crate::orchestration::push_execution_policy(policy);
        let result = active_sandbox_policy();
        crate::orchestration::pop_execution_policy();
        assert!(
            result.is_some(),
            "Worktree profile must keep sandbox dispatch active"
        );
    }

    #[test]
    fn toolchain_read_roots_resolve_from_env_vars_when_set() {
        let dir = tempfile::tempdir().unwrap();
        let uv = dir.path().join("uv-python");
        let cargo = dir.path().join("cargo-home");
        let rustup = dir.path().join("rustup-home");
        let goroot = dir.path().join("goroot");
        for d in [
            &uv,
            &cargo.join("bin"),
            &cargo.join("registry"),
            &rustup.join("toolchains"),
            &goroot,
        ] {
            std::fs::create_dir_all(d).unwrap();
        }

        let env = |key: &str| -> Option<PathBuf> {
            match key {
                "UV_PYTHON_INSTALL_DIR" => Some(uv.clone()),
                "CARGO_HOME" => Some(cargo.clone()),
                "RUSTUP_HOME" => Some(rustup.clone()),
                "GOROOT" => Some(goroot.clone()),
                _ => None,
            }
        };
        // No HOME fallback: only the env-resolved dirs should appear.
        let roots = toolchain_read_roots_from(env, None, |p| p.is_dir());

        assert!(
            roots.contains(&uv),
            "UV_PYTHON_INSTALL_DIR used whole: {roots:?}"
        );
        assert!(
            roots.contains(&cargo.join("bin")),
            "CARGO_HOME bin/ granted: {roots:?}"
        );
        assert!(
            roots.contains(&cargo.join("registry")),
            "CARGO_HOME registry/ (package cache cargo build reads) granted: {roots:?}"
        );
        assert!(
            !roots.contains(&cargo),
            "the cargo root (holding credentials.toml / config.toml) must not be granted: {roots:?}"
        );
        assert!(
            roots.contains(&rustup),
            "RUSTUP_HOME granted whole (settings.toml lives at the root, no credentials there): {roots:?}"
        );
        assert!(roots.contains(&goroot), "GOROOT used whole: {roots:?}");
    }

    #[test]
    fn toolchain_read_roots_fall_back_to_home_relative_dirs() {
        let home = tempfile::tempdir().unwrap();
        // Create a couple of standard home-relative toolchain trees.
        let uv = home.path().join(".local/share/uv/python");
        let pyenv_versions = home.path().join(".pyenv/versions");
        let cargo_bin = home.path().join(".cargo/bin");
        let go_bin = home.path().join("go/bin");
        for d in [&uv, &pyenv_versions, &cargo_bin, &go_bin] {
            std::fs::create_dir_all(d).unwrap();
        }

        // No env vars set; resolution must use the HOME fallbacks.
        let roots =
            toolchain_read_roots_from(|_| None, Some(home.path().to_path_buf()), |p| p.is_dir());

        assert!(roots.contains(&uv), "uv python tree via HOME: {roots:?}");
        assert!(
            roots.contains(&pyenv_versions),
            "pyenv narrowed to versions/ via HOME: {roots:?}"
        );
        assert!(
            roots.contains(&cargo_bin),
            "cargo bin via HOME (not all of ~/.cargo): {roots:?}"
        );
        assert!(roots.contains(&go_bin), "GOPATH bin via ~/go: {roots:?}");
        // ~/.nvm/versions does not exist, so it must be filtered out.
        assert!(
            !roots.iter().any(|r| r.ends_with(".nvm/versions")),
            "non-existent toolchain dirs must not bloat the profile: {roots:?}"
        );
    }

    #[test]
    fn toolchain_read_roots_only_include_existing_dirs() {
        let home = tempfile::tempdir().unwrap();
        // Nothing created under HOME and no env vars: zero roots.
        let roots =
            toolchain_read_roots_from(|_| None, Some(home.path().to_path_buf()), |p| p.is_dir());
        assert!(
            roots.is_empty(),
            "no toolchain dirs exist, so none should be granted: {roots:?}"
        );

        // GOROOT env-only: never falls back to HOME.
        let roots = toolchain_read_roots_from(|_| None, None, |_| true);
        assert!(
            roots.is_empty(),
            "with no HOME and no env vars, env-only GOROOT yields nothing: {roots:?}"
        );
    }
}

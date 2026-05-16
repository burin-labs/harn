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
use std::rc::Rc;

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

#[derive(Clone, Copy)]
pub(crate) enum FsAccess {
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
    vm.register_builtin("sandbox_active_backend", |_args, _out| {
        Ok(VmValue::String(Rc::from(active_backend_name())))
    });
    vm.register_builtin("sandbox_backend_available", |_args, _out| {
        Ok(VmValue::Bool(active_backend_available()))
    });
    vm.register_builtin("sandbox_active_profile", |_args, _out| {
        let profile = crate::orchestration::current_execution_policy()
            .map(|policy| policy.sandbox_profile)
            .unwrap_or(SandboxProfile::Unrestricted);
        Ok(VmValue::String(Rc::from(profile.as_str())))
    });
}

pub(crate) fn enforce_fs_path(builtin: &str, path: &Path, access: FsAccess) -> Result<(), VmError> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Ok(());
    };
    if matches!(policy.sandbox_profile, SandboxProfile::Unrestricted) {
        return Ok(());
    }
    if policy.workspace_roots.is_empty() {
        return Ok(());
    }
    let candidate = normalize_for_policy(path);
    let roots = normalized_workspace_roots(&policy);
    if roots.iter().any(|root| path_is_within(&candidate, root)) {
        return Ok(());
    }
    Err(sandbox_rejection(format!(
        "sandbox violation: builtin '{builtin}' attempted to {} '{}' outside workspace_roots [{}]",
        access.verb(),
        candidate.display(),
        roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

pub fn enforce_process_cwd(path: &Path) -> Result<(), VmError> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Ok(());
    };
    if matches!(policy.sandbox_profile, SandboxProfile::Unrestricted) {
        return Ok(());
    }
    if policy.workspace_roots.is_empty() {
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
            VmError::Thrown(crate::value::VmValue::String(std::rc::Rc::from(message)))
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
    VmError::Thrown(crate::value::VmValue::String(std::rc::Rc::from(format!(
        "process spawn failed: {error}"
    ))))
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
    policy
        .workspace_roots
        .iter()
        .map(|root| normalize_for_policy(&resolve_policy_path(root)))
        .collect()
}

pub(crate) fn process_sandbox_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    let roots = if policy.workspace_roots.is_empty() {
        vec![crate::stdlib::process::execution_root_path()]
    } else {
        normalized_workspace_roots(policy)
    };
    roots
        .into_iter()
        .map(|root| normalize_for_policy(&root))
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

#[cfg(any(target_os = "macos", target_os = "openbsd", target_os = "windows"))]
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
}

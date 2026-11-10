//! Process sandbox dispatch and per-platform OS confinement.
//!
//! The runtime exposes [`command_output`], [`std_command_for`],
//! [`tokio_command_for`], and the `enforce_*` helpers. A per-OS
//! [`SandboxBackend`] attaches the active capability ceiling:
//!
//! * **Linux** ([`linux::Backend`]): verified Landlock or bubblewrap filesystem
//!   scoping with default-deny seccomp, gated by `PR_SET_NO_NEW_PRIVS`.
//! * **macOS** ([`macos::Backend`]): a `sandbox-exec` profile rendered
//!   from the active capability set wraps the spawn.
//! * **OpenBSD** ([`openbsd::Backend`]): pledge/unveil applied via
//!   `pre_exec` on top of the standard `Command` plumbing.
//! * **Windows and every other platform** ([`backend::UnconfinedBackend`]):
//!   no OS sandbox. Children run unconfined; `os_hardened` refuses and the
//!   other confining profiles warn. See [`enforcement`].
//!
//! [`SandboxProfile`](crate::orchestration::SandboxProfile) sets how strictly [`CapabilityPolicy`] requires the backend:
//!
//! * `Unrestricted` — bypass everything (path enforcement and OS
//!   confinement).
//! * `Worktree` — workspace path enforcement; Linux requires a usable OS
//!   boundary, while platforms without a backend warn. Honors
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
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::orchestration::CapabilityPolicy;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use crate::orchestration::ProcessSandboxPreset;
#[cfg(test)]
use crate::orchestration::SandboxProfile;
use crate::value::{environment_io_error_thrown, ErrorCategory, VmError};

use paths::{
    access_is_exempt_from_scope, normalize_for_policy, path_is_within, relocated_runtime_roots,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use read_roots::developer_toolchain_cache_write_roots_for_home;
use read_roots::normalized_read_only_roots;
pub(crate) use read_roots::{
    developer_toolchain_read_roots_for_home, package_manager_config_read_roots_for_home,
    sandbox_user_home_dir,
};

mod backend;
mod build_command;
pub mod child_write;
pub(crate) use build_command::{build_std_command, build_tokio_command};
mod command_for;
pub use command_for::{
    session_std_command, session_tokio_command, std_command_for, std_command_for_with_env_state,
    tokio_command_for, tokio_command_for_with_env_state,
};
mod developer_git;
pub mod enforcement;
use enforcement::ensure_spawn_enforceable;
#[cfg(all(test, target_os = "linux"))]
mod enforcement_report;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod git_config;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) use git_config::process_sandbox_package_manager_config_read_roots;
mod handler_env;
mod introspection;
mod launch_environment;
pub use launch_environment::validate_command_environment;
#[cfg(target_os = "linux")]
pub(crate) mod linux;
mod locked_append;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "openbsd")]
mod openbsd;
mod path_scope;
pub(crate) use path_scope::enforce_fs_entry_path;
pub use path_scope::enforce_fs_path;
pub(crate) mod paths;
mod process_config;
mod process_output;
use path_scope::declared_path_roots_active;
#[cfg(test)]
use path_scope::with_declared_path_roots;
pub use path_scope::{
    check_fs_path_scope, check_git_metadata_path_scope, command_output_with_declared_roots,
};
mod read_roots;
mod refusal;
mod scope_memo;
mod scoped_fs;
use backend::ActiveBackend;
pub use backend::{
    active_backend_available, active_backend_filesystem_available,
    active_backend_filesystem_mechanism, active_backend_mechanism, active_backend_mechanism_limits,
    active_backend_name, conformance, host_contains_process_reads, self_confinement,
};
pub(crate) use backend::{PrepareOutcome, SandboxBackend};
use process_config::sandboxed_process_config;
pub use process_config::{
    apply_active_rustc_wrapper_policy, apply_active_rustc_wrapper_policy_for_command,
    prepare_active_rustc_wrapper_for_command, rustc_wrapper, ProcessCommandConfig, ProcessStdin,
};
use process_output::apply_process_config;
pub use process_output::{deterministic_message_locale_env, MESSAGE_LOCALE_OVERRIDE_ENV};
use scoped_fs::*;
pub(crate) use scoped_fs::{
    append_locked_scoped_at_open, append_scoped_at_open, copy_scoped_at_open,
    create_dir_scoped_at_open, rename_scoped_at_open,
};
pub(crate) mod process_cwd;
use process_cwd::enforce_process_cwd_for_policy;
pub(crate) use process_cwd::policy_process_cwd;
mod policy;
pub(crate) use policy::active_sandbox_policy;
mod policy_projection;
pub(crate) mod replay;
#[cfg(target_os = "linux")]
pub(crate) use policy_projection::process_sandbox_unix_socket_roots;
pub use policy_projection::unix_socket_enforcement;
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
pub(crate) use policy_projection::{
    process_sandbox_policy_read_roots, process_sandbox_policy_write_roots,
};
mod replace;

// Each backend uses one of these: platform helpers call `unavailable`; Linux confines in `pre_exec`.
/// A confinement an embedder can build here and enter in a process it re-execs.
///
/// Linux only, and deliberately so. This backend installs confinement from a
/// `pre_exec` callback, which nothing can carry across a process boundary; the
/// other backends put theirs in the spawn's argv, which survives on its own.
#[cfg(target_os = "linux")]
pub use linux::{decode_seccomp_hex, transferable_confinement, TransferableConfinement};
pub(crate) use refusal::unavailable;
pub use refusal::{
    infer_process_sandbox_mechanism, is_process_sandbox_signal, process_violation_error,
    HostProtectionGap, ProcessSandboxAssessment, ProcessSandboxDenialReporting,
    ProcessSandboxGrants, ProcessSandboxMechanism, ProcessSandboxNetwork, ProcessSandboxOperation,
    ProcessSandboxRefusal, ProcessSandboxReport, ProcessSandboxReportingContext, SandboxMechanism,
    SandboxMechanismAvailability, SandboxMechanismUnavailable, SandboxRequirement,
};
pub(crate) use refusal::{path_is_denied, process_sandbox_read_deny_roots};
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod toolchain_cache;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use toolchain_cache::process_roots as process_sandbox_developer_toolchain_cache_roots;
pub(crate) mod workspace_env;
#[cfg(all(test, unix))]
mod workspace_env_integration;

pub(crate) use handler_env::effective_fallback;
#[cfg(test)]
pub(crate) use handler_env::handler_sandbox_test_guard;
pub use introspection::{host_confinement, SANDBOX_CONFINEMENT_SCHEMA};
pub(crate) use locked_append::AppendLockOptions;
pub(crate) use policy::allows_network as policy_allows_network;
pub(crate) use replace::{
    apply_effect, atomic_replace_scoped_at_open_unlocked, atomic_write_scoped_at_open,
    read_for_replace_scoped_at_open,
};
pub(crate) use workspace_env::inject_workspace_process_env;
pub use workspace_env::{
    active_workspace_process_env, is_workspace_runtime_directory, workspace_local_tmpdir,
};
#[cfg(test)]
pub(crate) use workspace_env::{inject_workspace_tmpdir, TMPDIR_ENV_KEYS};

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
pub struct ProcessSandboxScope {
    pub workspace_roots: Vec<String>,
}

#[must_use]
pub struct ProcessSandboxScopeGuard {
    pushed: bool,
}

impl Drop for ProcessSandboxScopeGuard {
    fn drop(&mut self) {
        if self.pushed {
            crate::orchestration::pop_execution_policy();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SandboxFallback {
    Off,
    Warn,
    Enforce,
}

pub(crate) fn reset_sandbox_state() {
    WARNED_KEYS.with(|keys| keys.borrow_mut().clear());
}
harn_vm_core::stdlib_state_reset!(reset_sandbox_state);

/// A workspace-root scope violation: a path that resolved outside every
/// configured workspace root under a restricted [`SandboxProfile`](crate::orchestration::SandboxProfile).
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

pub fn enforce_process_cwd(path: &Path) -> Result<(), VmError> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Ok(());
    };
    enforce_process_cwd_for_policy(path, &policy)
}

pub fn push_process_sandbox_scope(
    scope: ProcessSandboxScope,
) -> Result<ProcessSandboxScopeGuard, VmError> {
    let Some(mut policy) = crate::orchestration::current_execution_policy() else {
        return Ok(ProcessSandboxScopeGuard { pushed: false });
    };
    if !policy.sandbox_profile.enforces_path_scope() {
        return Ok(ProcessSandboxScopeGuard { pushed: false });
    }

    let requested_roots: Vec<PathBuf> = scope
        .workspace_roots
        .iter()
        .filter_map(|root| {
            let trimmed = root.trim();
            (!trimmed.is_empty()).then(|| normalize_for_policy(&resolve_policy_path(trimmed)))
        })
        .collect();
    if requested_roots.is_empty() {
        return Ok(ProcessSandboxScopeGuard { pushed: false });
    }

    if !policy.workspace_roots.is_empty() {
        let ceiling_roots = normalized_workspace_roots(&policy);
        if let Some(rejected) = requested_roots.iter().find(|root| {
            !ceiling_roots
                .iter()
                .any(|ceiling| path_is_within(root, ceiling))
        }) {
            return Err(sandbox_rejection(format!(
                "sandbox violation: process sandbox workspace root '{}' is outside workspace_roots [{}]",
                rejected.display(),
                ceiling_roots
                    .iter()
                    .map(|root| root.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }

    // A per-command scope narrows the jail. Keeping a broader parent root
    // would let the command read its controller's files and sibling jobs.
    policy.workspace_roots = requested_roots
        .into_iter()
        .map(|root| root.display().to_string())
        .collect();
    crate::orchestration::push_execution_policy(policy);
    Ok(ProcessSandboxScopeGuard { pushed: true })
}

pub fn command_output(
    program: &str,
    args: &[String],
    config: &ProcessCommandConfig,
) -> Result<Output, VmError> {
    replay::check_supported()?;
    // Testbench replay mode short-circuits the spawn entirely.
    // Recording mode falls through; the duration is captured by the
    // recording handle below using the injected mock clock when one
    // is active.
    if let Some(intercepted) =
        crate::testbench::process_tape::intercept_spawn(program, args, config.cwd.as_deref())
    {
        return intercepted.map_err(|message| {
            VmError::Thrown(crate::value::VmValue::String(arcstr::ArcStr::from(message)))
        });
    }

    let recording =
        crate::testbench::process_tape::start_recording(program, args, config.cwd.as_deref());

    // Always rebuild through the command-aware resolver when a session env is
    // active so an earlier ambient `closed_env` cannot omit `for_command`
    // grants (harn#5549). `config.env` remains the overlay and still wins.
    let closed_config;
    let config = if let Some(env) =
        crate::stdlib::process::session_closed_env_for_command(program, config.env.iter().cloned())?
    {
        closed_config = ProcessCommandConfig {
            env,
            closed_env: true,
            ..config.clone()
        };
        &closed_config
    } else {
        config
    };

    let output = match active_sandbox_policy() {
        Some((policy, profile)) => {
            ensure_spawn_enforceable::<ActiveBackend>(&policy)?;
            let config = sandboxed_process_config(program, args, config, &policy)?;
            ActiveBackend::run_to_output(program, args, &config, &policy, profile)?
        }
        None => {
            let mut command = Command::new(program);
            command.args(args);
            apply_process_config(&mut command, config, None);
            // Interrupt-aware `Command::output()`: puts the child in its own
            // kill group and gracefully terminates the whole group when the
            // invoking scope is cancelled, a deadline fires, or the VM is
            // dropped. See `crate::op_interrupt`.
            crate::op_interrupt::capture_output_interruptible(&mut command).map_err(|error| {
                process_spawn_error(&error).unwrap_or_else(|| spawn_error(error))
            })?
        }
    };
    let refusal_command: Vec<String> = std::iter::once(program.to_string())
        .chain(args.iter().cloned())
        .collect();
    let refusal_cwd = config
        .cwd
        .as_deref()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    if let Some(error) = process_violation_error(&output, &refusal_command, &refusal_cwd) {
        return Err(error);
    }
    if let Some(span) = recording {
        span.finish(&output);
    }
    Ok(output)
}

pub fn process_spawn_error(error: &std::io::Error) -> Option<VmError> {
    let policy = crate::orchestration::current_execution_policy()?;
    if !policy.sandbox_profile.confines_processes() {
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
        return Some(sandbox_denial_error(
            format!("sandbox violation: process was denied by the OS sandbox before exec: {error}"),
            &message,
            &policy,
        ));
    }
    None
}

#[cfg(unix)]
fn sandbox_signal_status(output: &std::process::Output) -> bool {
    use std::os::unix::process::ExitStatusExt;

    is_process_sandbox_signal(output.status.signal())
}

#[cfg(not(unix))]
fn sandbox_signal_status(_output: &std::process::Output) -> bool {
    false
}

fn spawn_error(error: std::io::Error) -> VmError {
    environment_io_error_thrown(&error, format!("process spawn failed: {error}"))
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

/// Build the error for a process the OS sandbox blocked or killed. `detail` is
/// the denial evidence (child stderr/stdout, or the spawn `io::Error` text) the
/// OS produced — the only thing that names *which* path was refused.
///
/// The denial is reclassified from the default [`ErrorCategory::ToolRejected`]
/// to [`ErrorCategory::Environment`] only when it is provably an environment
/// gap: a developer-toolchain cache env var (`GOCACHE`, `CARGO_HOME`, …)
/// resolves OUTSIDE the sandbox jail AND `detail` actually names that path.
/// Requiring the path to appear in the denial text keeps a plain policy refusal
/// (e.g. a write outside the workspace) classified as `ToolRejected` even when
/// unrelated toolchain caches happen to sit outside this jail — the sandbox
/// correctly refused an action, which is a policy decision, not a provisioning
/// gap. When it IS an environment gap, the message names the offending root so
/// an embedder never reports it as the agent's code defect. Either way the
/// message points at the knobs that widen coverage.
fn sandbox_denial_error(summary: String, detail: &str, policy: &CapabilityPolicy) -> VmError {
    if let Some((var, path)) = toolchain_cache_gap_named_in_denial(policy, detail) {
        return VmError::CategorizedError {
            message: format!(
                "{summary}; the {var} toolchain cache resolves to '{}', which is outside the \
                 sandbox profile — a host environment/config gap, not the agent's code defect. \
                 For `harn run`, pass --sandbox-write-root '{}'; embedders can add it to \
                 process_sandbox.write_roots or extend the DeveloperToolchains preset",
                path.display(),
                path.display()
            ),
            category: ErrorCategory::Environment,
        };
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(path) = toolchain_cache_default_named_in_denial(policy, detail) {
        return VmError::CategorizedError {
            message: format!(
                "{summary}; the sandbox denied writing '{}', a well-known developer-toolchain \
                 cache outside the active profile — a host environment/config gap, not the \
                 agent's code defect. For `harn run`, pass --sandbox-write-root '{}'; embedders \
                 can enable the DeveloperToolchains preset or add process_sandbox.write_roots",
                path.display(),
                path.display()
            ),
            category: ErrorCategory::Environment,
        };
    }
    sandbox_rejection(sandbox_process_violation_message(summary))
}

fn sandbox_process_violation_message(summary: String) -> String {
    format!(
        "{summary}; if the command depends on a developer toolchain or cache outside the \
         workspace, pass --sandbox-read-root / --sandbox-write-root to `harn run`, or add the \
         root to process_sandbox.read_roots / process_sandbox.write_roots in an embedder policy"
    )
}

/// The read-granted root set the coverage check treats as "inside the jail":
/// the workspace write roots plus every read root the profile layers on
/// (Harn read-only mounts, process-only roots, developer-toolchain read/cache
/// roots, package-manager config roots). Built from the same single-owner
/// helpers the OS backends render from, so coverage cannot drift from what the
/// profile actually grants.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn coverage_jail_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    let mut roots = normalized_workspace_roots(policy);
    roots.extend(process_sandbox_roots(policy));
    roots.extend(process_sandbox_readonly_roots(policy));
    roots.extend(process_sandbox_policy_read_roots(policy));
    roots.extend(process_sandbox_policy_write_roots(policy));
    roots.extend(process_sandbox_developer_toolchain_read_roots(policy));
    roots.extend(process_sandbox_package_manager_config_read_roots(policy));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    roots.extend(process_sandbox_developer_toolchain_cache_roots(policy));
    roots
}

/// If a developer-toolchain *cache* env var is set to a path outside the
/// sandbox jail AND the denial evidence `detail` names that path, return
/// `(VAR, resolved_path)`. Both conditions are required: the out-of-jail cache
/// makes the gap possible, and the path appearing in the denial text is what
/// attributes *this* denial to it (so an unrelated refusal is not misread as an
/// environment gap just because some cache lives outside this jail). Read-only
/// install roots (`GOROOT`, `JAVA_HOME`, …) are intentionally excluded — they
/// usually sit under a system-preset prefix the jail set does not re-enumerate,
/// so flagging them would misclassify.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn toolchain_cache_gap_named_in_denial(
    policy: &CapabilityPolicy,
    detail: &str,
) -> Option<(String, PathBuf)> {
    let detail = detail.to_ascii_lowercase();
    let jail = coverage_jail_roots(policy);
    for name in crate::security::environment_policy::TOOLCHAIN_CACHE_ENV_VARS {
        let Some(value) = std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        let path = normalize_for_policy(Path::new(&value));
        let named = detail.contains(&path.to_string_lossy().to_ascii_lowercase());
        if named && !jail.iter().any(|root| path_is_within(&path, root)) {
            return Some(((*name).to_string(), path));
        }
    }
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn toolchain_cache_gap_named_in_denial(
    _policy: &CapabilityPolicy,
    _detail: &str,
) -> Option<(String, PathBuf)> {
    None
}

/// Fallback for toolchain caches that use their DEFAULT location (no env var
/// set), which [`toolchain_cache_gap_named_in_denial`] cannot see. If the denial
/// evidence names a well-known cache-write default (`~/Library/Caches/go-build`,
/// `~/.cargo/registry`, …) that is NOT inside the active jail — i.e. the
/// `DeveloperToolchains` preset is off or does not reach this policy — return the
/// path so the caller reclassifies to [`ErrorCategory::Environment`] instead of a
/// bare `ToolRejected`. When the preset IS active the cache is a jail root, so
/// the guard suppresses this and a genuine policy refusal stays `ToolRejected`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn toolchain_cache_default_named_in_denial(
    policy: &CapabilityPolicy,
    detail: &str,
) -> Option<PathBuf> {
    let home = sandbox_user_home_dir()?;
    let detail = detail.to_ascii_lowercase();
    let jail = coverage_jail_roots(policy);
    developer_toolchain_cache_write_roots_for_home(&home)
        .into_iter()
        .find(|root| {
            detail.contains(&root.to_string_lossy().to_ascii_lowercase())
                && !jail.iter().any(|jail_root| path_is_within(root, jail_root))
        })
}

/// Writable workspace roots derived from the active agent session's
/// workspace anchor: the anchor `primary` plus any `Extend` (writable)
/// mounts. Read-only mounts are intentionally excluded — they are not
/// writable jail roots (a read of one is permitted via the read-only-roots
/// path, but a write must not be). Returns `None` when there is no current
/// session or the session has no anchor, so the caller falls back to the
/// process execution root.
fn current_session_anchor_workspace_roots() -> Option<Vec<PathBuf>> {
    let session_id = crate::agent_sessions::current_session_id()?;
    let anchor = crate::agent_sessions::workspace_anchor(&session_id)?;
    let mut roots = vec![anchor.primary.clone()];
    for mounted in &anchor.additional_roots {
        if matches!(
            mounted.mount_mode,
            crate::workspace_anchor::MountMode::Extend
        ) {
            roots.push(mounted.path.clone());
        }
    }
    Some(roots)
}

/// The project root a run is bound to even when the OS process cwd differs.
/// Prefer the typed execution context and keep `HARN_PROJECT_ROOT` as the
/// legacy standalone fallback. This mirrors the `workspace.project_root` host
/// fallback so the write jail and reported project root agree.
fn project_root_workspace_root() -> Option<PathBuf> {
    crate::stdlib::process::project_root_path().or_else(|| {
        std::env::var("HARN_PROJECT_ROOT")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    })
}

fn normalized_workspace_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    let mut roots = base_workspace_roots(policy);
    let mut outside = path_scope::topology_extension_roots(&roots);
    outside.extend(workspace_env::granted_runtime_dir(policy, &roots));
    for dir in outside {
        if !roots.iter().any(|existing| existing == &dir) {
            roots.push(dir);
        }
    }
    roots
}

/// The workspace roots as configured by the policy (or the anchored/project/
/// execution-root fallback), before any git-topology extension. Kept separate
/// from [`normalized_workspace_roots`] so the git-topology detection runs
/// against the real project roots and never re-inspects the git dirs it adds.
fn base_workspace_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    if let Some(roots) = replay::workspace_roots() {
        return roots;
    }
    if policy.workspace_roots.is_empty() {
        // An empty `policy.workspace_roots` means no explicit write-jail was
        // configured for this call. Historically this fell straight back to the
        // process execution root, but under the eval pattern (process cwd !=
        // `--project`) and dispatch fan-out children, the process cwd is the
        // repo, not the project the run is bound to — so a write that correctly
        // resolved INTO the project was rejected as outside the jail
        // (HARN-CAP-201), the dispatched child wrote nothing, and the parent
        // silently compensated. Prefer, in order: (1) the active agent
        // session's workspace anchor (primary + writable `Extend` mounts) when
        // the session is anchored; (2) the typed execution project root, with
        // legacy `HARN_PROJECT_ROOT` as a fallback, robust across session
        // nesting that an unanchored dispatch child sees; (3) the process
        // execution root, the historical
        // default. Explicit `policy.workspace_roots` still take precedence
        // (handled in the non-empty branch below).
        if let Some(anchor_roots) = current_session_anchor_workspace_roots() {
            return anchor_roots
                .iter()
                .map(|root| normalize_for_policy(root))
                .collect();
        }
        if let Some(project_root) = project_root_workspace_root() {
            return vec![normalize_for_policy(&project_root)];
        }
        return vec![normalize_for_policy(
            &crate::stdlib::process::execution_root_path(),
        )];
    }
    read_roots::declared_workspace_roots(policy)
}

pub(crate) fn process_sandbox_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    normalized_workspace_roots(policy)
}

/// Merge the git-topology scope extension across every workspace `base_root`,
/// normalizing each discovered directory the same way as a configured root so
/// scope checks and dedup compare canonical paths. Both the OS sandbox backends
/// and the pure `check_fs_path_scope` enforcement consume the extended roots.
fn git_scope_extension_for_roots(
    base_roots: &[PathBuf],
) -> crate::stdlib::git_topology::GitScopeExtension {
    let mut merged = crate::stdlib::git_topology::GitScopeExtension::default();
    for root in base_roots {
        let ext = crate::stdlib::git_topology::git_scope_extension(root);
        for dir in ext.read_write {
            let dir = normalize_for_policy(&dir);
            if !merged.read_write.iter().any(|existing| existing == &dir) {
                merged.read_write.push(dir);
            }
        }
        for dir in ext.read_only {
            let dir = normalize_for_policy(&dir);
            if !merged.read_only.iter().any(|existing| existing == &dir) {
                merged.read_only.push(dir);
            }
        }
    }
    merged
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

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn process_sandbox_presets(policy: &CapabilityPolicy) -> Vec<ProcessSandboxPreset> {
    policy.process_sandbox.effective_presets()
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn process_sandbox_developer_toolchain_read_roots(
    policy: &CapabilityPolicy,
) -> Vec<PathBuf> {
    if !process_sandbox_presets(policy).contains(&ProcessSandboxPreset::DeveloperToolchains) {
        return Vec::new();
    }
    let Some(home) = sandbox_user_home_dir() else {
        return Vec::new();
    };
    developer_toolchain_read_roots_for_home(&home)
}

pub(crate) fn normalized_process_roots(roots: &[String]) -> Vec<PathBuf> {
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

/// Render one configured policy-root string to the exact path the sandbox jails
/// to — the single transform [`base_workspace_roots`] applies, exposed via
/// `crate::process_sandbox` so host disclosure and provenance surfaces report
/// the enforced jail path, not a pre-canonical approximation. Canonicalization
/// is best-effort for nonexistent paths (lexical fallback) and never panics.
pub fn render_policy_root(path: &str) -> PathBuf {
    normalize_for_policy(&resolve_policy_path(path))
}

/// Whether the OS profile rendered for a child grants its writable roots.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "openbsd"))]
pub(crate) fn policy_allows_child_writes(policy: &CapabilityPolicy) -> bool {
    policy.children_may_write()
}

#[cfg(target_os = "linux")]
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
mod tests;

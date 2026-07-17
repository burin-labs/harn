//! Process sandbox dispatch and per-platform OS confinement.
//!
//! The runtime exposes one stable surface — [`command_output`],
//! [`std_command_for`], [`tokio_command_for`], plus the
//! `enforce_*` helpers — and dispatches into a per-OS
//! [`SandboxBackend`] selected at compile time. The backend chooses
//! how to attach the active capability ceiling to the spawn:
//!
//! * **Linux** ([`linux::Backend`]): Landlock LSM filesystem scoping
//!   plus a default-deny seccomp-bpf syscall allowlist installed via
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
use std::io;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use crate::orchestration::ProcessSandboxPreset;
use crate::orchestration::{CapabilityPolicy, SandboxProfile};
use crate::value::{ErrorCategory, VmError, VmValue};
use crate::vm::Vm;

#[cfg(target_os = "linux")]
mod linux;
mod locked_append;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "openbsd")]
mod openbsd;
#[cfg(target_os = "windows")]
mod windows;

pub(crate) use locked_append::AppendLockOptions;

const HANDLER_SANDBOX_ENV: &str = "HARN_HANDLER_SANDBOX";
#[cfg(any(unix, windows))]
const MAX_SCOPED_PATH_COMPONENTS: usize = 256;

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
    /// When `true`, the child starts from an EMPTY environment and receives only
    /// the pairs in [`ProcessCommandConfig::env`] — the closed-by-construction
    /// path a session profile takes (`security::resolve_env` has already composed
    /// the allowlist + grants into `env`). When `false` (the default, legacy
    /// no-profile path), the child inherits the parent environment and `env` is
    /// overlaid on top.
    pub closed_env: bool,
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
        crate::op_interrupt::capture_output_interruptible(&mut command)
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
    Ok(VmValue::String(arcstr::ArcStr::from(active_backend_name())))
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
    Ok(VmValue::String(arcstr::ArcStr::from(profile.as_str())))
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

pub(crate) fn atomic_write_scoped_at_open(
    builtin: &str,
    path: &Path,
    contents: &[u8],
) -> io::Result<()> {
    let Some(target) = scoped_mutation_target(builtin, path, FsAccess::Write)? else {
        return atomic_write_unscoped(path, contents);
    };
    atomic_write_scoped_target(&target, contents)
}

pub(crate) fn append_scoped_at_open(builtin: &str, path: &Path, contents: &[u8]) -> io::Result<()> {
    let Some(target) = scoped_mutation_target(builtin, path, FsAccess::Write)? else {
        return append_unscoped(path, contents);
    };
    append_scoped_target(&target, contents)
}

pub(crate) fn append_locked_scoped_at_open(
    builtin: &str,
    path: &Path,
    contents: &[u8],
    options: AppendLockOptions,
) -> io::Result<()> {
    let Some(target) = scoped_mutation_target(builtin, path, FsAccess::Write)? else {
        return locked_append::append_locked_unscoped(path, contents, options);
    };
    locked_append::append_locked_scoped_target(&target, contents, options)
}

pub(crate) fn copy_scoped_at_open(builtin: &str, src: &Path, dst: &Path) -> io::Result<u64> {
    let Some(target) = scoped_mutation_target(builtin, dst, FsAccess::Write)? else {
        return std::fs::copy(src, dst);
    };
    copy_scoped_target(src, &target)
}

pub(crate) fn rename_scoped_at_open(builtin: &str, src: &Path, dst: &Path) -> io::Result<()> {
    let Some(src_target) = scoped_mutation_target(builtin, src, FsAccess::Delete)? else {
        return std::fs::rename(src, dst);
    };
    let dst_target = scoped_mutation_target(builtin, dst, FsAccess::Write)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "sandbox violation: builtin '{builtin}' attempted to rename '{}' without an active destination sandbox scope",
                dst.display()
            ),
        )
    })?;
    rename_scoped_targets(&src_target, &dst_target)
}

pub(crate) fn create_dir_scoped_at_open(
    builtin: &str,
    path: &Path,
    recursive: bool,
) -> io::Result<()> {
    let Some(target) = scoped_mutation_target(builtin, path, FsAccess::Write)? else {
        return if recursive {
            std::fs::create_dir_all(path)
        } else {
            std::fs::create_dir(path)
        };
    };
    if recursive {
        create_dir_all_scoped_target(&target)
    } else {
        create_dir_scoped_target(&target)
    }
}

#[derive(Clone, Debug)]
struct ScopedMutationTarget {
    root: PathBuf,
    relative: PathBuf,
}

fn scoped_mutation_target(
    builtin: &str,
    path: &Path,
    access: FsAccess,
) -> io::Result<Option<ScopedMutationTarget>> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Ok(None);
    };
    if matches!(policy.sandbox_profile, SandboxProfile::Unrestricted) {
        return Ok(None);
    }
    if is_standard_io_device_for_access(&normalize_io_device_path(path), access) {
        return Ok(None);
    }
    check_fs_path_scope(path, access).map_err(|violation| {
        io::Error::new(io::ErrorKind::PermissionDenied, violation.message(builtin))
    })?;
    let candidate = normalize_for_policy(path);
    let roots = normalized_workspace_roots(&policy);
    let Some(root) = roots
        .into_iter()
        .find(|root| path_is_within(&candidate, root))
    else {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "sandbox violation: builtin '{builtin}' attempted to {} '{}' outside writable workspace_roots",
                access.verb(),
                candidate.display()
            ),
        ));
    };
    let relative = candidate.strip_prefix(&root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "sandbox violation: builtin '{builtin}' attempted to {} '{}' outside workspace root '{}'",
                access.verb(),
                candidate.display(),
                root.display()
            ),
        )
    })?;
    if relative.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "sandbox violation: builtin '{builtin}' attempted to {} workspace root '{}'",
                access.verb(),
                root.display()
            ),
        ));
    }
    Ok(Some(ScopedMutationTarget {
        root,
        relative: relative.to_path_buf(),
    }))
}

fn atomic_write_unscoped(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = parent.unwrap_or_else(|| Path::new("."));
    // Restore the pre-hardening `mkdir -p` contract for content-producing
    // writes: an unrestricted (no active sandbox scope) write into a
    // not-yet-created directory recreates its ancestor chain, matching the
    // scoped path's `ensure_parent_dirs_scoped`.
    if let Some(parent) = parent {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_path = dir.join(scoped_tmp_name(path));
    let write_result = (|| -> io::Result<()> {
        let mut file = std::fs::File::create(&tmp_path)?;
        file.write_all(contents)?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(err) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

fn append_unscoped(path: &Path, contents: &[u8]) -> io::Result<()> {
    // Match the `append_file` contract: appending to a new log in a
    // not-yet-created directory recreates the parent chain.
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(contents))
}

fn scoped_tmp_name(path: &Path) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    format!(".{file_name}.harn-tmp.{}.{counter}", std::process::id())
}

#[cfg(unix)]
fn atomic_write_scoped_target(target: &ScopedMutationTarget, contents: &[u8]) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    // Content-producing writes recreate their parent chain (`mkdir -p`),
    // restoring the pre-hardening `write_file`/`http_download` contract that
    // downstream `.harn` relies on. The creation stays inside the scope root
    // and reuses the same symlink-safe parent-fd walk as the write itself.
    let (parent, file_name) = ensure_parent_dirs_scoped(target)?;
    let tmp_name = scoped_tmp_name(Path::new(&file_name));
    let mut file = openat_file(
        parent.as_raw_fd(),
        &tmp_name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0o666,
    )?;
    let write_result = (|| -> io::Result<()> {
        file.write_all(contents)?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(err) = write_result {
        let _ = unlinkat_name(parent.as_raw_fd(), &tmp_name, 0);
        return Err(err);
    }
    if let Err(err) = renameat_name(
        parent.as_raw_fd(),
        &tmp_name,
        parent.as_raw_fd(),
        &file_name,
    ) {
        let _ = unlinkat_name(parent.as_raw_fd(), &tmp_name, 0);
        return Err(err);
    }
    sync_dir_fd(parent.as_raw_fd());
    Ok(())
}

#[cfg(windows)]
fn atomic_write_scoped_target(target: &ScopedMutationTarget, contents: &[u8]) -> io::Result<()> {
    let (parent, file_name) = win_scoped_parent(target, true)?;
    let full = parent.join(&file_name);
    win_reject_reparse_leaf(&full)?;
    atomic_write_unscoped(&full, contents)
}

#[cfg(all(not(unix), not(windows)))]
fn atomic_write_scoped_target(target: &ScopedMutationTarget, contents: &[u8]) -> io::Result<()> {
    let full = target.root.join(&target.relative);
    if let Some(parent) = full.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write_unscoped(&full, contents)
}

#[cfg(unix)]
fn append_scoped_target(target: &ScopedMutationTarget, contents: &[u8]) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    // Append creates the file (and its parent chain) when absent, matching the
    // pre-hardening `append_file` contract (append-to-a-new-log-in-a-new-dir).
    let (parent, file_name) = ensure_parent_dirs_scoped(target)?;
    let mut file = openat_file(
        parent.as_raw_fd(),
        &file_name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0o666,
    )?;
    file.write_all(contents)
}

#[cfg(windows)]
fn append_scoped_target(target: &ScopedMutationTarget, contents: &[u8]) -> io::Result<()> {
    let (parent, file_name) = win_scoped_parent(target, true)?;
    let full = parent.join(&file_name);
    win_reject_reparse_leaf(&full)?;
    append_unscoped(&full, contents)
}

#[cfg(all(not(unix), not(windows)))]
fn append_scoped_target(target: &ScopedMutationTarget, contents: &[u8]) -> io::Result<()> {
    let full = target.root.join(&target.relative);
    if let Some(parent) = full.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    append_unscoped(&full, contents)
}

#[cfg(unix)]
fn copy_scoped_target(src: &Path, target: &ScopedMutationTarget) -> io::Result<u64> {
    use std::os::fd::AsRawFd;

    let mut source = std::fs::File::open(src)?;
    let source_metadata = source.metadata().ok();
    let (parent, file_name) = open_parent_dir_scoped(target)?;
    let mut destination = openat_file(
        parent.as_raw_fd(),
        &file_name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0o666,
    )?;
    let copied = io::copy(&mut source, &mut destination)?;
    destination.sync_all()?;
    if let Some(metadata) = source_metadata {
        let _ = destination.set_permissions(metadata.permissions());
    }
    sync_dir_fd(parent.as_raw_fd());
    Ok(copied)
}

#[cfg(windows)]
fn copy_scoped_target(src: &Path, target: &ScopedMutationTarget) -> io::Result<u64> {
    // Copy destinations keep the "parent must already exist" contract, so the
    // walk does not auto-create (create_parents = false), matching the unix
    // `open_parent_dir_scoped` path.
    let (parent, file_name) = win_scoped_parent(target, false)?;
    let full = parent.join(&file_name);
    win_reject_reparse_leaf(&full)?;
    std::fs::copy(src, full)
}

#[cfg(all(not(unix), not(windows)))]
fn copy_scoped_target(src: &Path, target: &ScopedMutationTarget) -> io::Result<u64> {
    std::fs::copy(src, target.root.join(&target.relative))
}

#[cfg(unix)]
fn rename_scoped_targets(src: &ScopedMutationTarget, dst: &ScopedMutationTarget) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let (src_parent, src_name) = open_parent_dir_scoped(src)?;
    let (dst_parent, dst_name) = open_parent_dir_scoped(dst)?;
    renameat_name(
        src_parent.as_raw_fd(),
        &src_name,
        dst_parent.as_raw_fd(),
        &dst_name,
    )?;
    sync_dir_fd(dst_parent.as_raw_fd());
    Ok(())
}

#[cfg(windows)]
fn rename_scoped_targets(src: &ScopedMutationTarget, dst: &ScopedMutationTarget) -> io::Result<()> {
    // No `win_reject_reparse_leaf` on the leaves here: rename operates on the
    // directory entry (the name), not by traversing through the target, and may
    // legitimately move/replace a reparse point. The junction-traversal defense
    // is the ancestor-chain validation in `win_scoped_parent`.
    let (src_parent, src_name) = win_scoped_parent(src, false)?;
    let (dst_parent, dst_name) = win_scoped_parent(dst, false)?;
    std::fs::rename(src_parent.join(&src_name), dst_parent.join(&dst_name))
}

#[cfg(all(not(unix), not(windows)))]
fn rename_scoped_targets(src: &ScopedMutationTarget, dst: &ScopedMutationTarget) -> io::Result<()> {
    std::fs::rename(src.root.join(&src.relative), dst.root.join(&dst.relative))
}

#[cfg(unix)]
fn create_dir_scoped_target(target: &ScopedMutationTarget) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let (parent, file_name) = open_parent_dir_scoped(target)?;
    mkdirat_name(parent.as_raw_fd(), &file_name)?;
    sync_dir_fd(parent.as_raw_fd());
    Ok(())
}

#[cfg(windows)]
fn create_dir_scoped_target(target: &ScopedMutationTarget) -> io::Result<()> {
    // Single `mkdir` keeps the "parent must already exist" contract; only the
    // leaf is created, after verifying no ancestor is a junction/symlink. No
    // `win_reject_reparse_leaf` on the leaf: `CreateDirectoryW` creates a NEW
    // name and fails `AlreadyExists` if anything (reparse point or not) already
    // occupies it — it never writes *through* an existing leaf — so the
    // ancestor-chain validation is the whole defense.
    let (parent, file_name) = win_scoped_parent(target, false)?;
    win_create_dir_raw(&parent.join(&file_name))
}

#[cfg(all(not(unix), not(windows)))]
fn create_dir_scoped_target(target: &ScopedMutationTarget) -> io::Result<()> {
    std::fs::create_dir(target.root.join(&target.relative))
}

#[cfg(unix)]
fn create_dir_all_scoped_target(target: &ScopedMutationTarget) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let root = open_dir_absolute(&target.root)?;
    let mut current = root;
    for component in clean_relative_components(&target.relative)? {
        match open_dir_at(current.as_raw_fd(), &component) {
            Ok(next) => current = next,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                mkdirat_name(current.as_raw_fd(), &component)?;
                let next = open_dir_at(current.as_raw_fd(), &component)?;
                current = next;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn create_dir_all_scoped_target(target: &ScopedMutationTarget) -> io::Result<()> {
    // `mkdir -p`: every component (including the leaf) is created, and each is
    // verified not to be a reparse point (junction/symlink) as the walk descends.
    let components = win_clean_relative_components(&target.relative)?;
    win_walk_components(&target.root, &components, true)?;
    Ok(())
}

#[cfg(all(not(unix), not(windows)))]
fn create_dir_all_scoped_target(target: &ScopedMutationTarget) -> io::Result<()> {
    std::fs::create_dir_all(target.root.join(&target.relative))
}

#[cfg(unix)]
/// Create the ancestor directory chain of a scoped write/append target,
/// mirroring the pre-hardening `mkdir -p` behavior of the content-producing
/// filesystem builtins (`write_file`, `write_file_bytes`, `append_file`,
/// `append_file_locked`) and `http_download`. Only the ancestors are created —
/// the final path component
/// is the file the caller writes. Traversal stays scoped to `target.root` and
/// symlink-safe (each level is opened with `O_NOFOLLOW` via `open_dir_at`), so
/// this preserves the security properties #4147 added while restoring the
/// directory-autovivification contract downstream code depends on. Concurrent
/// creators are tolerated (a losing `mkdirat` that sees `EEXIST` is ignored).
///
/// The returned parent fd is the one content-producing callers must use for
/// their final `openat`/`renameat`, so the path is not resolved again between
/// mkdir-p and the write.
///
/// Structural operations (copy destination, rename, remove, single `mkdir`)
/// intentionally do NOT call this — they keep `open_parent_dir_scoped`'s
/// "parent must already exist" semantics.
#[cfg(unix)]
fn ensure_parent_dirs_scoped(
    target: &ScopedMutationTarget,
) -> io::Result<(std::os::fd::OwnedFd, String)> {
    use std::os::fd::AsRawFd;

    let mut components = clean_relative_components(&target.relative)?;
    let file_name = components.pop().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "sandbox scoped open requires a file name: {}",
                target.relative.display()
            ),
        )
    })?;
    let root = open_dir_absolute(&target.root)?;
    let mut current = root;
    for component in components {
        match open_dir_at(current.as_raw_fd(), &component) {
            Ok(next) => current = next,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Err(mkerr) = mkdirat_name(current.as_raw_fd(), &component) {
                    if mkerr.kind() != io::ErrorKind::AlreadyExists {
                        return Err(mkerr);
                    }
                }
                current = open_dir_at(current.as_raw_fd(), &component)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok((current, file_name))
}

#[cfg(unix)]
fn open_parent_dir_scoped(
    target: &ScopedMutationTarget,
) -> io::Result<(std::os::fd::OwnedFd, String)> {
    use std::os::fd::AsRawFd;

    let mut components = clean_relative_components(&target.relative)?;
    let file_name = components.pop().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "sandbox scoped open requires a file name: {}",
                target.relative.display()
            ),
        )
    })?;
    let root = open_dir_absolute(&target.root)?;
    let mut current = root;
    for component in components {
        current = open_dir_at(current.as_raw_fd(), &component)?;
    }
    Ok((current, file_name))
}

#[cfg(unix)]
fn clean_relative_components(path: &Path) -> io::Result<Vec<String>> {
    use std::os::unix::ffi::OsStrExt;

    let mut out = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let bytes = value.as_bytes();
                if bytes.contains(&0) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("path component contains NUL: {}", path.display()),
                    ));
                }
                out.push(value.to_string_lossy().into_owned());
                if out.len() > MAX_SCOPED_PATH_COMPONENTS {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "sandbox scoped path exceeds {MAX_SCOPED_PATH_COMPONENTS} components: {}",
                            path.display()
                        ),
                    ));
                }
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("sandbox scoped path must stay relative: {}", path.display()),
                ));
            }
        }
    }
    Ok(out)
}

#[cfg(unix)]
fn open_dir_absolute(path: &Path) -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path contains NUL: {}", path.display()),
        )
    })?;
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(unix)]
fn open_dir_at(parent_fd: libc::c_int, name: &str) -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::{FromRawFd, OwnedFd};

    let c_name = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            parent_fd,
            c_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(unix)]
fn openat_file(
    parent_fd: libc::c_int,
    name: &str,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<std::fs::File> {
    use std::os::fd::FromRawFd;

    let c_name = c_name(name)?;
    let fd = unsafe { libc::openat(parent_fd, c_name.as_ptr(), flags, mode as libc::c_uint) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn mkdirat_name(parent_fd: libc::c_int, name: &str) -> io::Result<()> {
    let c_name = c_name(name)?;
    let rc = unsafe { libc::mkdirat(parent_fd, c_name.as_ptr(), 0o777) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn renameat_name(
    old_parent_fd: libc::c_int,
    old_name: &str,
    new_parent_fd: libc::c_int,
    new_name: &str,
) -> io::Result<()> {
    let old_name = c_name(old_name)?;
    let new_name = c_name(new_name)?;
    let rc = unsafe {
        libc::renameat(
            old_parent_fd,
            old_name.as_ptr(),
            new_parent_fd,
            new_name.as_ptr(),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn unlinkat_name(parent_fd: libc::c_int, name: &str, flags: libc::c_int) -> io::Result<()> {
    let c_name = c_name(name)?;
    let rc = unsafe { libc::unlinkat(parent_fd, c_name.as_ptr(), flags) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn sync_dir_fd(fd: libc::c_int) {
    let _ = unsafe { libc::fsync(fd) };
}

#[cfg(unix)]
fn c_name(name: &str) -> io::Result<std::ffi::CString> {
    std::ffi::CString::new(name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path component contains NUL: {name:?}"),
        )
    })
}

// ---------------------------------------------------------------------------
// Windows scoped-walk primitives (junction/symlink-safe directory descent).
//
// Windows has no `openat`, and `O_NOFOLLOW` has no equivalent that a plain
// `std::fs` path open honors — worse, a *junction* (mount-point reparse point)
// IS a directory and is creatable by a non-admin user, so it slips past every
// "is this a symlink" check that only inspects the leaf. The unix path defends
// the whole chain by opening each component `O_NOFOLLOW`; the Windows path here
// mirrors that by opening each walked component with
// `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS` (so the reparse
// point itself is opened, not its target) and refusing the walk the moment any
// component reports a mount-point or symlink reparse tag. See
// research/scoped-fs-mkdir-footguns (#12, RedirectionGuard) for the class.
//
// Residual, Windows-CI-only: because there is no handle-relative openat here,
// each component is re-resolved by string as the walk descends, so a
// concurrent attacker who swaps an *already-validated* ancestor for a junction
// between our check and the next open is not fully closed (the unix fd-walk is;
// the true fix is `NtCreateFile` with a `RootDirectory` handle). The
// intermediate-junction class the acceptance test covers IS closed.
// ---------------------------------------------------------------------------

/// Reparse tags Windows assigns to the two "traverses out of the tree"
/// reparse-point kinds the scoped walk must refuse. Defined locally so the
/// module does not need the `Win32_System_SystemServices` feature just for two
/// stable ABI constants.
#[cfg(windows)]
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
#[cfg(windows)]
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

#[cfg(windows)]
fn win_wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Refuse `path` if it is a mount-point or symlink reparse point. The handle is
/// opened with `FILE_FLAG_OPEN_REPARSE_POINT` so we inspect the reparse point
/// itself rather than following it, and `FILE_FLAG_BACKUP_SEMANTICS` so a
/// directory handle is permitted. A `NotFound` error is propagated unchanged so
/// callers can distinguish "does not exist yet" (create it) from "exists and is
/// hostile" (refuse).
#[cfg(windows)]
fn win_reject_reparse_point(path: &Path) -> io::Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FileAttributeTagInfo, GetFileInformationByHandleEx,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };

    // Query attributes only; no read/write access to the object is needed.
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;

    let wide = win_wide(path);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            std::ptr::from_mut(&mut info).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    let result = if ok == 0 {
        Err(io::Error::last_os_error())
    } else if info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        && matches!(
            info.ReparseTag,
            IO_REPARSE_TAG_MOUNT_POINT | IO_REPARSE_TAG_SYMLINK
        )
    {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "sandbox scoped walk refuses reparse-point (junction/symlink) component: {}",
                path.display()
            ),
        ))
    } else {
        Ok(())
    };
    unsafe {
        CloseHandle(handle);
    }
    result
}

/// A reparse point that squats on a *leaf* target name is refused; a leaf that
/// simply does not exist yet is fine (the caller is about to create it).
#[cfg(windows)]
fn win_reject_reparse_leaf(path: &Path) -> io::Result<()> {
    match win_reject_reparse_point(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Low-level `CreateDirectoryW` used by the scoped walk. Kept raw (rather than
/// `std::fs::create_dir`) so the recurrence-guard lint can assert the scoped
/// Windows walk never reaches for a path-based `std::fs` mutation.
#[cfg(windows)]
fn win_create_dir_raw(path: &Path) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
    let wide = win_wide(path);
    let ok = unsafe { CreateDirectoryW(wide.as_ptr(), std::ptr::null()) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Windows analogue of [`clean_relative_components`]: reject `..`, absolute, and
/// drive-prefixed components, cap the depth, and refuse embedded NULs — keeping
/// the same invariants the unix walk enforces before descending.
#[cfg(windows)]
fn win_clean_relative_components(path: &Path) -> io::Result<Vec<std::ffi::OsString>> {
    use std::os::windows::ffi::OsStrExt;

    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                if value.encode_wide().any(|unit| unit == 0) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("path component contains NUL: {}", path.display()),
                    ));
                }
                out.push(value.to_os_string());
                if out.len() > MAX_SCOPED_PATH_COMPONENTS {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "sandbox scoped path exceeds {MAX_SCOPED_PATH_COMPONENTS} components: {}",
                            path.display()
                        ),
                    ));
                }
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("sandbox scoped path must stay relative: {}", path.display()),
                ));
            }
        }
    }
    Ok(out)
}

/// Descend `root` through `components`, refusing any component that is a
/// junction/symlink reparse point. When `create` is set, missing directories
/// are created (`mkdir -p`) and re-validated immediately, so a directory we
/// just made cannot be a reparse point. Returns the validated deepest path.
#[cfg(windows)]
fn win_walk_components(
    root: &Path,
    components: &[std::ffi::OsString],
    create: bool,
) -> io::Result<PathBuf> {
    // The configured workspace root is trusted, but verify it resolves to a real
    // directory and is not itself a reparse point, mirroring the unix
    // `open_dir_absolute` `O_NOFOLLOW` open of the root.
    win_reject_reparse_point(root)?;
    let mut current = root.to_path_buf();
    for component in components {
        current.push(component);
        match win_reject_reparse_point(&current) {
            Ok(()) => {}
            Err(err) if create && err.kind() == io::ErrorKind::NotFound => {
                match win_create_dir_raw(&current) {
                    Ok(()) => {}
                    // A concurrent creator won the race; tolerate and re-validate.
                    Err(mkerr) if mkerr.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(mkerr) => return Err(mkerr),
                }
                win_reject_reparse_point(&current)?;
            }
            Err(err) => return Err(err),
        }
    }
    Ok(current)
}

/// Validate the ancestor chain of a scoped target on Windows and return the
/// verified `(parent_dir, leaf_name)`. With `create_parents`, missing ancestors
/// are created; without it, the parent must already exist (structural ops).
#[cfg(windows)]
fn win_scoped_parent(
    target: &ScopedMutationTarget,
    create_parents: bool,
) -> io::Result<(PathBuf, std::ffi::OsString)> {
    let mut components = win_clean_relative_components(&target.relative)?;
    let file_name = components.pop().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "sandbox scoped open requires a file name: {}",
                target.relative.display()
            ),
        )
    })?;
    let parent = win_walk_components(&target.root, &components, create_parents)?;
    Ok((parent, file_name))
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
    if matches!(policy.sandbox_profile, SandboxProfile::Unrestricted) {
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

    let mut merged_roots = if policy.workspace_roots.is_empty() {
        Vec::new()
    } else {
        normalized_workspace_roots(&policy)
    };
    for requested in requested_roots {
        if !merged_roots
            .iter()
            .any(|existing| path_is_within(&requested, existing))
        {
            merged_roots.push(requested);
        }
    }
    policy.workspace_roots = merged_roots
        .into_iter()
        .map(|root| root.display().to_string())
        .collect();
    crate::orchestration::push_execution_policy(policy);
    Ok(ProcessSandboxScopeGuard { pushed: true })
}

fn enforce_process_cwd_for_policy(path: &Path, policy: &CapabilityPolicy) -> Result<(), VmError> {
    if matches!(policy.sandbox_profile, SandboxProfile::Unrestricted) {
        return Ok(());
    }
    let candidate = normalize_for_policy(path);
    let roots = normalized_workspace_roots(policy);
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
            VmError::Thrown(crate::value::VmValue::String(arcstr::ArcStr::from(message)))
        });
    }

    let recording =
        crate::testbench::process_tape::start_recording(program, args, config.cwd.as_deref());

    let output = match active_sandbox_policy() {
        Some((policy, profile)) => {
            let config = sandboxed_process_config(config, &policy)?;
            ActiveBackend::run_to_output(program, args, &config, &policy, profile)?
        }
        None => {
            let mut command = Command::new(program);
            command.args(args);
            apply_process_config(&mut command, config);
            // Interrupt-aware `Command::output()`: puts the child in its own
            // kill group and gracefully terminates the whole group when the
            // invoking scope is cancelled, a deadline fires, or the VM is
            // dropped. See `crate::op_interrupt`.
            crate::op_interrupt::capture_output_interruptible(&mut command).map_err(|error| {
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

fn sandboxed_process_config(
    config: &ProcessCommandConfig,
    policy: &CapabilityPolicy,
) -> Result<ProcessCommandConfig, VmError> {
    let mut resolved = config.clone();
    if let Some(cwd) = resolved.cwd.as_ref() {
        enforce_process_cwd_for_policy(cwd, policy)?;
    } else {
        resolved.cwd = Some(default_process_cwd_for_policy(policy)?);
    }
    neutralize_rustc_wrapper(&mut resolved.env);
    inject_workspace_tmpdir(&mut resolved.env, policy);
    Ok(resolved)
}

/// Disable any Cargo `rustc` wrapper (e.g. `sccache`) for a sandboxed spawn.
///
/// `sccache` is a single shared, long-lived per-user daemon. If a sandboxed
/// cargo build is the first caller to spawn it, the daemon inherits the
/// `sandbox-exec` confinement permanently — even after it reparents to
/// launchd — and then fails *every* later build machine-wide with
/// `Operation not permitted` (it can no longer read build inputs outside the
/// sandbox root nor write its cache dir under `~/Library/Caches`). A
/// per-command sandbox must never be allowed to poison a cross-workspace
/// daemon, so sandboxed builds bypass the wrapper entirely. Cargo treats an
/// empty `CARGO_BUILD_RUSTC_WRAPPER` / `RUSTC_WRAPPER` as "no wrapper", which
/// overrides any `build.rustc-wrapper` set in `.cargo/config.toml`. The
/// on-disk cache and all unsandboxed builds are unaffected.
fn neutralize_rustc_wrapper(env: &mut Vec<(String, String)>) {
    for key in ["RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER"] {
        if let Some(entry) = env.iter_mut().find(|(existing, _)| existing == key) {
            entry.1.clear();
        } else {
            env.push((key.to_string(), String::new()));
        }
    }
}

/// Workspace-relative directory name for the sandbox-writable temp dir that
/// [`workspace_local_tmpdir`] points `TMPDIR`/`TMP`/`TEMP` at. Lives inside a
/// writable workspace root (which both OS backends already grant) so any
/// toolchain that honors `TMPDIR` writes its intermediates somewhere the
/// sandbox permits, instead of the unwritable system `/tmp`.
pub(crate) const WORKSPACE_TMPDIR_NAME: &str = ".harn-tmp";

/// The environment keys a workspace-local temp dir is exported under. `TMPDIR`
/// is the POSIX/Rust/clang/gcc/Go/Swift convention; `TMP`/`TEMP` cover tools
/// (and Windows toolchains) that read those instead.
pub(crate) const TMPDIR_ENV_KEYS: [&str; 3] = ["TMPDIR", "TMP", "TEMP"];

/// Resolve the sandbox-writable, workspace-local temp directory for `policy`,
/// creating it lazily.
///
/// Compiler linkers (`rustc`/`cc`/`ld`, Go, Swift, …) and countless other
/// toolchains write intermediate object/temp files to `$TMPDIR`, defaulting to
/// the system `/tmp` when it is unset. Under a restricted profile `/tmp` is
/// outside the writable workspace roots, so those writes are denied and a build
/// that would otherwise succeed FALSE-FAILS for an infrastructure reason. By
/// pointing the child's temp dir at a directory *inside* the first writable
/// workspace root — which the OS sandbox already grants write access to — the
/// build's temp writes land somewhere permitted without widening the sandbox.
///
/// Returns `None` when the policy declares no writable workspace root (there is
/// nowhere sandbox-writable to anchor the temp dir) or when the directory could
/// not be created (the caller then leaves the child's inherited temp dir
/// untouched rather than failing the spawn).
pub(crate) fn workspace_local_tmpdir(policy: &CapabilityPolicy) -> Option<PathBuf> {
    let root = normalized_workspace_roots(policy).into_iter().next()?;
    let tmpdir = root.join(WORKSPACE_TMPDIR_NAME);
    if let Err(error) = std::fs::create_dir_all(&tmpdir) {
        warn_once(
            "handler_sandbox_workspace_tmpdir",
            &format!(
                "could not create workspace-local temp dir '{}': {error}; \
                 leaving the child's inherited temp dir in place",
                tmpdir.display()
            ),
        );
        return None;
    }
    // Keep the temp dir's churn out of every git-based diff/status (so it never
    // leaks into an agent's view, a PR, or eval grading) by self-ignoring its
    // own contents. A `.gitignore` of `*` inside the dir excludes everything,
    // including itself, regardless of whether the workspace tracks it. Written
    // best-effort and only when absent so we don't thrash an existing file.
    let ignore = tmpdir.join(".gitignore");
    if !ignore.exists() {
        let _ = std::fs::write(
            &ignore,
            "# Created by the Harn sandbox; safe to delete.\n*\n",
        );
    }
    Some(tmpdir)
}

/// Overlay `TMPDIR`/`TMP`/`TEMP` onto a child's env so a sandboxed toolchain
/// writes its intermediates to a workspace-local, sandbox-writable directory
/// instead of the unwritable system `/tmp` (see [`workspace_local_tmpdir`]).
///
/// A key the caller set explicitly in `env` is left untouched — an intentional
/// per-call `TMPDIR` is honored. The inherited-from-parent value is *not*
/// preserved: that is exactly the non-writable `/tmp` (or empty) we must
/// override. No-op under an unrestricted/absent policy or when no writable
/// workspace root is available.
pub(crate) fn inject_workspace_tmpdir(env: &mut Vec<(String, String)>, policy: &CapabilityPolicy) {
    if matches!(policy.sandbox_profile, SandboxProfile::Unrestricted) {
        return;
    }
    let Some(tmpdir) = workspace_local_tmpdir(policy) else {
        return;
    };
    let tmpdir = tmpdir.display().to_string();
    for key in TMPDIR_ENV_KEYS {
        if env.iter().any(|(existing, _)| existing == key) {
            // The caller pinned this key explicitly; respect it.
            continue;
        }
        env.push((key.to_string(), tmpdir.clone()));
    }
}

/// The `TMPDIR`/`TMP`/`TEMP` overrides for the *currently active* execution
/// policy, as `(key, value)` pairs, or an empty vec when no restricted policy
/// is active or no writable workspace root exists.
///
/// This reads the active execution policy directly (gating only on a restricted
/// `sandbox_profile`), deliberately *not* through [`active_sandbox_policy`]:
/// the workspace-local temp dir is a benefit of the child env, independent of
/// whether OS confinement is enforced, so it must still engage under
/// `HARN_HANDLER_SANDBOX=warn`/`off` (which only weaken *enforcement*, not the
/// profile). [`inject_workspace_tmpdir`] still no-ops under `Unrestricted`.
///
/// This is the entry point the `host_call("process", …)` exec/spawn builder and
/// the `harn-hostlib` real spawner use to overlay the keys onto a
/// `Command`/`tokio::process::Command`, skipping any the caller already pinned.
pub fn active_workspace_tmpdir_env() -> Vec<(String, String)> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Vec::new();
    };
    let mut env = Vec::new();
    inject_workspace_tmpdir(&mut env, &policy);
    env
}

/// Environment overlay that pins a child tool's *message* output to a
/// deterministic, English, UTF-8-preserving locale, as `(key, value)` pairs.
///
/// Build/test/verify commands inherit the parent environment, so a user whose
/// shell sets `LC_ALL=ja_JP.UTF-8` (or `LANG=de_DE.UTF-8`) would otherwise get
/// *localized* compiler/test output. Every downstream matcher that keys on
/// English diagnostics — deterministic syntax repair, error-signature
/// grounding, completion/pass-fail classification — would then silently
/// misfire for a non-Anglosphere user. Forcing a stable message locale is the
/// root-cause fix: it keeps the English matchers correct by construction,
/// without shipping per-locale translations of every toolchain.
///
/// `LC_MESSAGES=C` forces untranslated (English) messages for gettext-based
/// tools (gcc/clang, git-l10n, GNU coreutils, gradle) while deliberately *not*
/// touching `LC_CTYPE`/`LANG`, so UTF-8 handling of non-ASCII source and
/// identifiers is preserved (unlike the blunt `LC_ALL=C`, which forces an ASCII
/// ctype and can mangle non-ASCII identifiers in diagnostics). The .NET CLI
/// ignores `LC_*` and localizes from its own variable / the OS UI language, so
/// `DOTNET_CLI_UI_LANGUAGE=en` is required in addition.
///
/// A user-inherited `LC_ALL` would override `LC_MESSAGES`, so the spawn sites
/// additionally strip `LC_ALL` (unless the caller pinned it) before applying
/// this overlay. Both are subject to the caller-pinned-key rule (like the
/// `TMPDIR` overlay): an explicit `env`/`env_remove` still wins.
pub fn deterministic_message_locale_env() -> Vec<(String, String)> {
    vec![
        ("LC_MESSAGES".to_string(), "C".to_string()),
        ("DOTNET_CLI_UI_LANGUAGE".to_string(), "en".to_string()),
    ]
}

/// The environment variable a user-inherited value of which would override
/// [`deterministic_message_locale_env`]'s `LC_MESSAGES`. Spawn sites strip this
/// (unless the caller pinned it) so the forced message locale actually takes
/// effect. Kept as a named constant so both spawn paths stay in sync.
pub const MESSAGE_LOCALE_OVERRIDE_ENV: &str = "LC_ALL";

fn default_process_cwd_for_policy(policy: &CapabilityPolicy) -> Result<PathBuf, VmError> {
    let roots = normalized_workspace_roots(policy);
    let current = std::env::current_dir().map_err(|error| {
        VmError::Thrown(crate::value::VmValue::String(arcstr::ArcStr::from(
            format!("process cwd resolution failed: {error}"),
        )))
    })?;
    let current = normalize_for_policy(&current);
    if roots.iter().any(|root| path_is_within(&current, root)) {
        return Ok(current);
    }
    roots.first().cloned().ok_or_else(|| {
        VmError::Thrown(crate::value::VmValue::String(arcstr::ArcStr::from(
            "process cwd resolution failed: no workspace root available",
        )))
    })
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
        return Some(sandbox_rejection(sandbox_process_violation_message(
            format!(
                "sandbox violation: process was denied by the OS sandbox (status {})",
                output.status.code().unwrap_or(-1)
            ),
        )));
    }
    if sandbox_signal_status(output) {
        return Some(sandbox_rejection(sandbox_process_violation_message(
            format!(
                "sandbox violation: process was terminated by the OS sandbox (status {})",
                output.status
            ),
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
        return Some(sandbox_rejection(sandbox_process_violation_message(
            format!("sandbox violation: process was denied by the OS sandbox before exec: {error}"),
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
    // A profile-governed session builds a closed environment: clear the
    // inherited parent env first so the child sees ONLY what the resolver
    // admitted (allowlist + grants), already materialized in `config.env`.
    // The default no-profile path leaves the inherited env in place and overlays
    // `config.env` on top, preserving legacy behavior.
    if config.closed_env {
        command.env_clear();
    }
    command.envs(config.env.iter().map(|(key, value)| (key, value)));
    if config.stdin_null {
        command.stdin(Stdio::null());
    }
}

fn spawn_error(error: std::io::Error) -> VmError {
    VmError::Thrown(crate::value::VmValue::String(arcstr::ArcStr::from(
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

fn sandbox_process_violation_message(summary: String) -> String {
    format!(
        "{summary}; if the command depends on a user-managed toolchain or cache outside the workspace, add that root to process_sandbox.read_roots or process_sandbox.write_roots"
    )
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
    // Git keeps a linked worktree's real git dir and shared common dir outside
    // the working tree; both need read-write scope or every git subprocess
    // fails inside an otherwise ordinary worktree checkout. See
    // [`crate::stdlib::git_topology`].
    for dir in git_scope_extension_for_roots(&roots).read_write {
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
    let mut roots: Vec<PathBuf> = policy
        .read_only_roots
        .iter()
        .map(|root| normalize_for_policy(&resolve_policy_path(root)))
        .collect();
    // Object stores borrowed through `objects/info/alternates` (e.g. a
    // `git clone --shared`) live outside the workspace and are only ever read
    // by git; grant them read-only scope. See [`crate::stdlib::git_topology`].
    for dir in git_scope_extension_for_roots(&base_workspace_roots(policy)).read_only {
        if !roots.iter().any(|existing| existing == &dir) {
            roots.push(dir);
        }
    }
    roots
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

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn process_sandbox_package_manager_config_read_roots(
    policy: &CapabilityPolicy,
) -> Vec<PathBuf> {
    if !process_sandbox_presets(policy).contains(&ProcessSandboxPreset::PackageManagerConfig) {
        return Vec::new();
    }
    let Some(home) = sandbox_user_home_dir() else {
        return Vec::new();
    };
    package_manager_config_read_roots_for_home(&home)
}

/// Per-user toolchain *cache* roots that JVM/iOS build tools read **and write**
/// while a sandboxed build runs (Gradle, Maven, CocoaPods, Xcode, Kotlin
/// Native). Unlike [`developer_toolchain_read_roots_for_home`] these are not
/// read-only: a build legitimately populates `~/.gradle/caches`,
/// `~/.m2/repository`, `~/Library/Developer/Xcode/DerivedData`, etc. They are
/// gated on the `DeveloperToolchains` preset and granted *write* only when the
/// active policy already permits workspace writes (mirroring `UserTemp`); under
/// a read-only policy they fall back to read access so dependency resolution
/// still works.
// Cache *write* roots are only consumed by the macOS (seatbelt) and Linux
// (Landlock) sandbox backends; the Windows backend deliberately does not grant
// recursive home-scoped cache roots (see `windows.rs`). Gating to those two
// targets keeps `-D warnings` happy on Windows, where this would otherwise be
// dead code.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn process_sandbox_developer_toolchain_cache_roots(
    policy: &CapabilityPolicy,
) -> Vec<PathBuf> {
    if !process_sandbox_presets(policy).contains(&ProcessSandboxPreset::DeveloperToolchains) {
        return Vec::new();
    }
    let Some(home) = sandbox_user_home_dir() else {
        return Vec::new();
    };
    developer_toolchain_cache_write_roots_for_home(&home)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn sandbox_user_home_dir() -> Option<PathBuf> {
    // Only an absolute home grounds the user-scope read-roots below; a
    // relative or unset home yields no extra roots (the safe direction).
    crate::user_dirs::home_dir().filter(|path| path.is_absolute())
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn developer_toolchain_read_roots_for_home(home: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<_> = [
        ".asdf",
        ".bun",
        ".cargo",
        ".fnm",
        ".juliaup",
        ".local/bin",
        ".local/share/mise",
        ".local/share/uv",
        ".nvm",
        ".pyenv",
        ".rbenv",
        ".rustup",
        ".sdkman",
        ".swiftly",
        ".volta",
        "go",
    ]
    .into_iter()
    .map(|entry| normalize_for_policy(&home.join(entry)))
    .collect();
    #[cfg(target_os = "windows")]
    roots.extend(
        [
            "AppData/Local/Programs/Python",
            "AppData/Local/uv",
            "AppData/Roaming/uv",
            "scoop",
        ]
        .into_iter()
        .map(|entry| normalize_for_policy(&home.join(entry))),
    );
    roots.sort_unstable();
    roots.dedup();
    roots
}

/// Per-user JVM/iOS toolchain cache roots (read+write). Kept platform-shared so
/// the macOS seatbelt and Linux Landlock backends render the same set; the
/// macOS-only `~/Library/...` entries are simply absent on Linux disk and the
/// `optional`/NotFound handling in each backend skips roots that do not exist.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn developer_toolchain_cache_write_roots_for_home(home: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<_> = [
        ".gradle",                             // Gradle (JVM/Android/Kotlin)
        ".m2",                                 // Maven (JVM)
        ".konan",                              // Kotlin/Native
        "Library/Caches/CocoaPods",            // CocoaPods (iOS/macOS)
        "Library/Developer/Xcode/DerivedData", // Xcode build products
    ]
    .into_iter()
    .map(|entry| normalize_for_policy(&home.join(entry)))
    .collect();
    roots.sort_unstable();
    roots.dedup();
    roots
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn package_manager_config_read_roots_for_home(home: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<_> = [
        ".npmrc",
        ".gitconfig",
        ".netrc",
        ".yarnrc.yml",
        ".config",
        ".npm",
        ".cache",
        ".pip",
        ".pypirc",
        ".cargo/config",
        ".cargo/config.toml",
        ".cargo/credentials",
        ".cargo/credentials.toml",
        ".cargo/registry",
        ".cargo/git",
    ]
    .into_iter()
    .map(|entry| normalize_for_policy(&home.join(entry)))
    .collect();
    roots.sort_unstable();
    roots.dedup();
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
    use crate::tool_annotations::SideEffectLevel;
    policy
        .side_effect_level
        .as_ref()
        .map(|level| SideEffectLevel::rank_str(level) >= SideEffectLevel::Network.rank())
        .unwrap_or(true)
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "openbsd",
    target_os = "windows"
))]
pub(crate) fn policy_allows_workspace_write(policy: &CapabilityPolicy) -> bool {
    !policy.capabilities_are_restricted()
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
mod tests;

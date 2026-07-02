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

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
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

pub fn enforce_process_cwd(path: &Path) -> Result<(), VmError> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Ok(());
    };
    enforce_process_cwd_for_policy(path, &policy)
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

/// The host-declared project root (`HARN_PROJECT_ROOT`), if set and
/// non-empty. This mirrors the standalone project-root fallback the
/// `workspace.project_root` host capability already uses
/// (`stdlib::host`), so the write jail and the reported project root
/// agree. It is the project a run is bound to even when the OS process
/// cwd differs (the eval harness runs `burin-headless` from the repo with
/// `--project <fixture>`, and the real IDE may launch from elsewhere).
fn project_root_env_workspace_root() -> Option<PathBuf> {
    std::env::var("HARN_PROJECT_ROOT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn normalized_workspace_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
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
        // the session is anchored; (2) the host-declared `HARN_PROJECT_ROOT`
        // project root (robust across the session nesting that an unanchored
        // dispatch child sees); (3) the process execution root, the historical
        // default. Explicit `policy.workspace_roots` still take precedence
        // (handled in the non-empty branch below).
        if let Some(anchor_roots) = current_session_anchor_workspace_roots() {
            return anchor_roots
                .iter()
                .map(|root| normalize_for_policy(root))
                .collect();
        }
        if let Some(project_root) = project_root_env_workspace_root() {
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
        // Serialize env mutation and clear HARN_PROJECT_ROOT so this asserts the
        // pure execution-root fallback (the project-root-env preference is
        // covered by the next test).
        let _env_lock = crate::runtime_paths::test_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::env::remove_var("HARN_PROJECT_ROOT");
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

    /// Regression for burin-labs/burin-code#3288. When a restricted policy has
    /// no explicit `workspace_roots`, the write jail must follow the
    /// host-declared `HARN_PROJECT_ROOT` project — NOT the process/execution
    /// cwd. This is the eval/dispatch pattern: `burin-headless` runs from the
    /// repo (`execution cwd = repo`) with `--project <fixture>` + matching
    /// `HARN_PROJECT_ROOT`, and a dispatched sub-agent worker's writes resolve
    /// into the fixture. Before the fix the empty-roots fallback used the
    /// execution cwd (the repo), so the in-project write was rejected
    /// (HARN-CAP-201) and the dispatched child wrote nothing.
    #[test]
    fn empty_workspace_roots_prefer_project_root_env_over_execution_root() {
        let _env_lock = crate::runtime_paths::test_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let project = tempfile::tempdir().unwrap();
        let execution_cwd = tempfile::tempdir().unwrap();
        std::env::set_var("HARN_PROJECT_ROOT", project.path());
        crate::stdlib::process::set_thread_execution_context(Some(
            crate::orchestration::RunExecutionRecord {
                cwd: Some(execution_cwd.path().to_string_lossy().into_owned()),
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

        // A write that resolves INTO the project is allowed even though the
        // process/execution cwd is elsewhere.
        assert!(
            enforce_fs_path(
                "write_file",
                &project.path().join("test/created.ts"),
                FsAccess::Write,
            )
            .is_ok(),
            "write into HARN_PROJECT_ROOT must be allowed"
        );
        // A write under the execution cwd (the repo, in the eval pattern) is NOT
        // the project and must still be rejected — the jail moved to the
        // project, it did not widen to both.
        assert!(
            enforce_fs_path(
                "write_file",
                &execution_cwd.path().join("escape.ts"),
                FsAccess::Write,
            )
            .is_err(),
            "write under the execution cwd (outside the project) must be rejected"
        );

        pop_execution_policy();
        crate::stdlib::process::set_thread_execution_context(None);
        std::env::remove_var("HARN_PROJECT_ROOT");
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
    fn sandboxed_process_config_defaults_cwd_to_current_when_allowed() {
        let cwd = std::env::current_dir().unwrap();
        let policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![cwd.to_string_lossy().into_owned()],
            ..CapabilityPolicy::default()
        };

        let resolved = sandboxed_process_config(&ProcessCommandConfig::default(), &policy).unwrap();

        assert_eq!(resolved.cwd.unwrap(), normalize_for_policy(&cwd));
    }

    #[test]
    fn sandboxed_process_config_defaults_cwd_to_workspace_when_current_is_outside() {
        let workspace = tempfile::tempdir().unwrap();
        let policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
            ..CapabilityPolicy::default()
        };

        let resolved = sandboxed_process_config(&ProcessCommandConfig::default(), &policy).unwrap();

        assert_eq!(
            resolved.cwd.unwrap(),
            normalize_for_policy(workspace.path())
        );
    }

    #[test]
    fn sandboxed_process_config_rejects_explicit_cwd_outside_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
            ..CapabilityPolicy::default()
        };
        let config = ProcessCommandConfig {
            cwd: Some(outside.path().to_path_buf()),
            ..ProcessCommandConfig::default()
        };

        assert!(sandboxed_process_config(&config, &policy).is_err());
    }

    #[test]
    fn sandboxed_process_config_neutralizes_rustc_wrapper() {
        let cwd = std::env::current_dir().unwrap();
        let policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![cwd.to_string_lossy().into_owned()],
            ..CapabilityPolicy::default()
        };

        // A sandboxed spawn must bypass sccache so it can never spawn (and
        // thereby permanently confine) the shared daemon.
        let resolved = sandboxed_process_config(&ProcessCommandConfig::default(), &policy).unwrap();
        let env: std::collections::BTreeMap<_, _> = resolved.env.into_iter().collect();
        assert_eq!(env.get("RUSTC_WRAPPER").map(String::as_str), Some(""));
        assert_eq!(
            env.get("CARGO_BUILD_RUSTC_WRAPPER").map(String::as_str),
            Some("")
        );
    }

    #[test]
    fn neutralize_rustc_wrapper_overrides_caller_supplied_wrapper() {
        // Even if a caller (or inherited env) asked for sccache, the sandboxed
        // config forces it off rather than appending a duplicate entry.
        let mut env = vec![
            ("RUSTC_WRAPPER".to_string(), "sccache".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
        ];
        neutralize_rustc_wrapper(&mut env);
        let collected: std::collections::BTreeMap<_, _> = env.iter().cloned().collect();
        assert_eq!(collected.get("RUSTC_WRAPPER").map(String::as_str), Some(""));
        assert_eq!(
            collected
                .get("CARGO_BUILD_RUSTC_WRAPPER")
                .map(String::as_str),
            Some("")
        );
        assert_eq!(collected.get("PATH").map(String::as_str), Some("/usr/bin"));
        // No duplicate RUSTC_WRAPPER entries.
        assert_eq!(env.iter().filter(|(k, _)| k == "RUSTC_WRAPPER").count(), 1);
    }

    #[test]
    fn workspace_local_tmpdir_lands_inside_the_first_writable_root() {
        let workspace = tempfile::tempdir().unwrap();
        let policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
            ..CapabilityPolicy::default()
        };

        let tmpdir = workspace_local_tmpdir(&policy).expect("a writable root yields a temp dir");

        // The temp dir is created, lives under the writable workspace root, and
        // is named by the documented convention.
        assert!(tmpdir.is_dir(), "temp dir must be created: {tmpdir:?}");
        assert!(
            path_is_within(&tmpdir, &normalize_for_policy(workspace.path())),
            "temp dir {tmpdir:?} must be inside the writable workspace root"
        );
        assert!(tmpdir.ends_with(WORKSPACE_TMPDIR_NAME));
        // It self-ignores so its churn never shows in a git diff.
        let ignore = std::fs::read_to_string(tmpdir.join(".gitignore")).unwrap_or_default();
        assert!(
            ignore.lines().any(|line| line.trim() == "*"),
            "temp dir must carry a self-ignoring .gitignore, got {ignore:?}"
        );
        // It is within the sandbox's writable scope: a write under it passes the
        // same path-scope check the OS sandbox enforces.
        push_execution_policy(policy);
        assert!(
            check_fs_path_scope(&tmpdir.join("rustcXXXX/intermediate.o"), FsAccess::Write).is_ok(),
            "writes under the workspace-local temp dir must be in sandbox scope"
        );
        pop_execution_policy();
    }

    #[test]
    fn inject_workspace_tmpdir_is_a_noop_under_unrestricted_profile() {
        // The unrestricted profile short-circuits the injection helper: an
        // unsandboxed child keeps whatever TMPDIR it would otherwise inherit.
        let policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Unrestricted,
            workspace_roots: vec!["/definitely/not/writable/xyzzy".to_string()],
            ..CapabilityPolicy::default()
        };
        let mut env = Vec::new();
        inject_workspace_tmpdir(&mut env, &policy);
        assert!(
            env.is_empty(),
            "unrestricted profile must not inject a TMPDIR override, got {env:?}"
        );
    }

    #[test]
    fn inject_workspace_tmpdir_sets_all_three_keys_inside_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
            ..CapabilityPolicy::default()
        };
        let mut env = Vec::new();
        inject_workspace_tmpdir(&mut env, &policy);

        let collected: std::collections::BTreeMap<_, _> = env.into_iter().collect();
        let expected = workspace_local_tmpdir(&policy)
            .unwrap()
            .display()
            .to_string();
        for key in TMPDIR_ENV_KEYS {
            assert_eq!(
                collected.get(key).map(String::as_str),
                Some(expected.as_str()),
                "{key} must point at the workspace-local temp dir"
            );
        }
    }

    #[test]
    fn deterministic_message_locale_env_forces_english_utf8_safe_messages() {
        let env: std::collections::BTreeMap<_, _> =
            deterministic_message_locale_env().into_iter().collect();
        // gettext tools (gcc/clang, git-l10n, coreutils, gradle) honor
        // LC_MESSAGES; `C` yields untranslated English.
        assert_eq!(env.get("LC_MESSAGES").map(String::as_str), Some("C"));
        // .NET ignores LC_* and localizes from its own variable.
        assert_eq!(
            env.get("DOTNET_CLI_UI_LANGUAGE").map(String::as_str),
            Some("en")
        );
        // Deliberately NOT setting LC_ALL/LC_CTYPE/LANG so UTF-8 handling of
        // non-ASCII source and identifiers is preserved (unlike `LC_ALL=C`).
        assert!(
            !env.contains_key("LC_ALL"),
            "must not force LC_ALL (would clobber UTF-8 ctype)"
        );
        assert!(!env.contains_key("LC_CTYPE"));
        assert!(!env.contains_key("LANG"));
        // The override-strip constant names the one variable that would defeat
        // LC_MESSAGES if inherited.
        assert_eq!(MESSAGE_LOCALE_OVERRIDE_ENV, "LC_ALL");
    }

    #[test]
    fn inject_workspace_tmpdir_respects_a_caller_pinned_tmpdir() {
        let workspace = tempfile::tempdir().unwrap();
        let policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
            ..CapabilityPolicy::default()
        };
        // Caller already pinned TMPDIR; only the untouched siblings get filled.
        let mut env = vec![("TMPDIR".to_string(), "/caller/explicit/tmp".to_string())];
        inject_workspace_tmpdir(&mut env, &policy);

        let collected: std::collections::BTreeMap<_, _> = env.iter().cloned().collect();
        assert_eq!(
            collected.get("TMPDIR").map(String::as_str),
            Some("/caller/explicit/tmp"),
            "an explicit caller TMPDIR must be preserved untouched"
        );
        let expected = workspace_local_tmpdir(&policy)
            .unwrap()
            .display()
            .to_string();
        assert_eq!(
            collected.get("TMP").map(String::as_str),
            Some(expected.as_str())
        );
        assert_eq!(
            collected.get("TEMP").map(String::as_str),
            Some(expected.as_str())
        );
        // And no duplicate TMPDIR entry was appended.
        assert_eq!(env.iter().filter(|(k, _)| k == "TMPDIR").count(), 1);
    }

    #[test]
    fn sandboxed_process_config_injects_workspace_tmpdir() {
        let workspace = tempfile::tempdir().unwrap();
        let policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
            ..CapabilityPolicy::default()
        };
        let config = ProcessCommandConfig {
            cwd: Some(workspace.path().to_path_buf()),
            ..ProcessCommandConfig::default()
        };
        let resolved = sandboxed_process_config(&config, &policy).unwrap();
        let env: std::collections::BTreeMap<_, _> = resolved.env.into_iter().collect();
        let expected = workspace_local_tmpdir(&policy)
            .unwrap()
            .display()
            .to_string();
        assert_eq!(
            env.get("TMPDIR").map(String::as_str),
            Some(expected.as_str()),
            "the command_output path must inject a workspace-local TMPDIR"
        );
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

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn developer_toolchain_roots_cover_common_home_managed_runtimes() {
        let temp_home = tempfile::tempdir().expect("temp home");
        let roots = developer_toolchain_read_roots_for_home(temp_home.path());
        let normalized_home = normalize_for_policy(temp_home.path());

        for suffix in [
            Path::new(".cargo"),
            Path::new(".rustup"),
            Path::new(".pyenv"),
            Path::new(".nvm"),
            Path::new(".volta"),
            Path::new(".local/share/uv"),
            Path::new("go"),
        ] {
            assert!(
                roots.iter().any(|path| path.ends_with(suffix)),
                "expected a developer-toolchain grant for {}",
                suffix.display()
            );
        }
        assert!(
            roots.iter().all(|path| path.starts_with(&normalized_home)),
            "developer-toolchain roots must stay under HOME"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn developer_toolchain_cache_roots_cover_jvm_and_ios_toolchains() {
        let temp_home = tempfile::tempdir().expect("temp home");
        let roots = developer_toolchain_cache_write_roots_for_home(temp_home.path());
        let normalized_home = normalize_for_policy(temp_home.path());

        for suffix in [
            Path::new(".gradle"),
            Path::new(".m2"),
            Path::new(".konan"),
            Path::new("Library/Caches/CocoaPods"),
            Path::new("Library/Developer/Xcode/DerivedData"),
        ] {
            assert!(
                roots.iter().any(|path| path.ends_with(suffix)),
                "expected a JVM/iOS toolchain cache grant for {}",
                suffix.display()
            );
        }
        assert!(
            roots.iter().all(|path| path.starts_with(&normalized_home)),
            "toolchain cache roots must stay under HOME"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn developer_toolchain_cache_roots_require_developer_toolchains_preset() {
        let mut policy = CapabilityPolicy {
            workspace_roots: vec!["/tmp/harn-workspace".to_string()],
            ..CapabilityPolicy::default()
        };
        // Default presets include DeveloperToolchains -> cache roots present
        // (only when an absolute HOME is resolvable on this host).
        if sandbox_user_home_dir().is_some() {
            assert!(
                !process_sandbox_developer_toolchain_cache_roots(&policy).is_empty(),
                "default presets should render JVM/iOS cache roots"
            );
        }
        // Explicitly dropping DeveloperToolchains removes them.
        policy.process_sandbox.presets = Some(vec![ProcessSandboxPreset::SystemRuntime]);
        assert!(
            process_sandbox_developer_toolchain_cache_roots(&policy).is_empty(),
            "cache roots must be gated on the DeveloperToolchains preset"
        );
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

//! Process abstraction trait used by `tools/proc` and
//! `tools/long_running`.
//!
//! Tier 1C of the de-flake epic (#1057). Production code spawns through
//! the [`ProcessSpawner`] trait — the default implementation in
//! `process::real` wraps `std::process::Child` and goes through
//! `harn_vm::process_sandbox`. Tests install a `MockSpawner` (see
//! `process::mock`) that returns deterministic [`MockProcess`] handles,
//! so process-tool tests no longer depend on real subprocess scheduling
//! or wall-clock timing.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Resolved exit information for a finished process. Mirrors the subset of
/// `std::process::ExitStatus` that the process-tool builtins surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitStatus {
    /// Exit code from `exit(2)` / `_exit(2)`. `None` means the process did not
    /// exit normally (it was terminated by a signal).
    pub code: Option<i32>,
    /// Unix signal that terminated the process, when applicable. `None` on
    /// non-Unix targets or when the process exited normally.
    pub signal: Option<i32>,
}

impl ExitStatus {
    /// Construct a normal exit with the given code.
    pub fn from_code(code: i32) -> Self {
        Self {
            code: Some(code),
            signal: None,
        }
    }

    /// Construct a signal-terminated exit.
    pub fn from_signal(signal: i32) -> Self {
        Self {
            code: None,
            signal: Some(signal),
        }
    }
}

/// How a spawn should treat the parent's environment. Mirrors the legacy
/// `EnvMode` from `tools/proc.rs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvMode {
    /// Inherit the parent's environment, then apply `env` overrides.
    InheritClean,
    /// Clear the environment, then apply `env`.
    Replace,
    /// Inherit the parent's environment and apply `env` (default behaviour).
    Patch,
}

/// Parameters describing a single spawn. The spawner is responsible for any
/// sandbox setup (Linux seccomp/landlock, macOS sandbox-exec, etc.) and for
/// configuring the child's process group when requested.
#[derive(Clone, Debug)]
pub struct SpawnSpec {
    /// Builtin name surfaced in error messages (e.g. `"hostlib_tools_run_command"`).
    pub builtin: &'static str,
    /// Program to execute. Must be non-empty (validated by the spawner).
    pub program: String,
    /// Arguments to pass to the program.
    pub args: Vec<String>,
    /// Working directory for the child. `None` inherits the parent's cwd.
    pub cwd: Option<PathBuf>,
    /// Environment overrides to apply (interpretation depends on `env_mode`).
    pub env: BTreeMap<String, String>,
    /// How to treat the parent's environment.
    pub env_mode: EnvMode,
    /// Whether stdin will be written to (`true`) or piped to /dev/null (`false`).
    pub use_stdin: bool,
    /// Set the child's process group to its own pid (`setpgid(0, 0)`). Used
    /// for long-running handles so the kill-by-pgid path works.
    pub configure_process_group: bool,
}

/// Handle to a running (or finished) process. Used by both the synchronous
/// `proc::run` path and the long-running waiter thread.
///
/// The trait is intentionally small: the legacy code already managed
/// stdout/stderr drain on dedicated threads, and stdin is written once after
/// spawn — wrapping those reads/writes via boxed trait objects keeps the
/// real and mock paths uniform without forcing async into the rest of the
/// hostlib.
pub trait ProcessHandle: Send {
    /// OS process id, when available.
    fn pid(&self) -> Option<u32>;

    /// OS process group id, when available. Falls back to [`Self::pid`] on
    /// platforms that don't expose process groups.
    fn process_group_id(&self) -> Option<u32>;

    /// Returns a killer that can terminate the process even after the
    /// stdout/stderr/wait halves have been moved into the waiter thread.
    fn killer(&self) -> Arc<dyn ProcessKiller>;

    /// Take ownership of the stdin pipe, if the spawn requested one.
    fn take_stdin(&mut self) -> Option<Box<dyn Write + Send>>;

    /// Take ownership of the stdout reader.
    fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>>;

    /// Take ownership of the stderr reader.
    fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>>;

    /// Wait for the process to exit, optionally with a timeout. Returns
    /// `(Some(status), false)` when the process exited cleanly,
    /// `(None, true)` when the timeout elapsed (and the spawner killed the
    /// child), or `(None, false)` when the wait failed for a reason other
    /// than the timeout.
    fn wait_with_timeout(
        &mut self,
        timeout: Option<Duration>,
    ) -> io::Result<(Option<ExitStatus>, bool)>;

    /// Block until the process exits, no timeout.
    fn wait(&mut self) -> io::Result<ExitStatus>;
}

/// Kill side of a [`ProcessHandle`]. Cloneable via `Arc` so cancellation
/// works after the waiter thread has taken ownership of the handle itself.
pub trait ProcessKiller: Send + Sync {
    /// Send SIGKILL to the process (and its process group, when applicable).
    fn kill(&self);
}

/// Spawner abstraction: produces [`ProcessHandle`] instances.
pub trait ProcessSpawner: Send + Sync {
    /// Spawn the configured process.
    fn spawn(&self, spec: SpawnSpec) -> Result<Box<dyn ProcessHandle>, ProcessError>;
}

/// Errors raised by a spawner. These map onto `HostlibError::Backend` /
/// `HostlibError::InvalidParameter` at the call site so the script-side
/// surface stays unchanged.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ProcessError {
    /// `argv` was empty or otherwise malformed.
    #[error("invalid argv: {0}")]
    InvalidArgv(String),
    /// Sandbox setup (e.g. landlock policy assembly) failed.
    #[error("sandbox setup failed: {0}")]
    SandboxSetup(String),
    /// Sandbox rejected the supplied cwd.
    #[error("sandbox cwd rejected: {0}")]
    SandboxCwd(String),
    /// Sandbox rejected the spawn at execve time.
    #[error("sandbox rejected spawn: {0}")]
    SandboxSpawn(String),
    /// Generic spawn failure (typically io::Error from `Command::spawn`).
    #[error("spawn failed: {0}")]
    Spawn(String),
}

use std::cell::RefCell;

thread_local! {
    static THREAD_SPAWNER: RefCell<Option<Arc<dyn ProcessSpawner>>> = const { RefCell::new(None) };
}

/// Install a per-thread spawner used by `spawn_process` from this thread.
/// Returns a guard that restores the previous spawner on drop. Tests use
/// this to install a [`super::mock::MockSpawner`]; production never calls
/// it (the default real spawner runs whenever no per-thread spawner is
/// installed).
///
/// Thread-local rather than global so parallel test execution is safe.
/// Process-tool spawns happen on the test's thread; the long-running
/// waiter threads operate on the handle that was already returned, so
/// they don't perform spawner lookups themselves.
pub fn install_spawner(spawner: Arc<dyn ProcessSpawner>) -> SpawnerGuard {
    let prev = THREAD_SPAWNER.with(|slot| slot.replace(Some(spawner)));
    SpawnerGuard { prev: Some(prev) }
}

/// Guard returned by [`install_spawner`]. Restores the previous spawner on
/// drop so installs nest correctly across tests.
pub struct SpawnerGuard {
    // Outer Option distinguishes "guard already restored" (None) from
    // "guard owes a restore" (Some(_)); inner Option carries the previous
    // spawner slot value (which can itself be None when no spawner was set).
    #[allow(clippy::option_option)]
    prev: Option<Option<Arc<dyn ProcessSpawner>>>,
}

impl Drop for SpawnerGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.take() {
            THREAD_SPAWNER.with(|slot| {
                *slot.borrow_mut() = prev;
            });
        }
    }
}

/// Return the currently installed spawner for this thread, falling back
/// to the default real spawner.
pub fn current_spawner() -> Arc<dyn ProcessSpawner> {
    THREAD_SPAWNER
        .with(|slot| slot.borrow().clone())
        .unwrap_or_else(super::real::default_spawner)
}

/// Spawn a process via the currently installed spawner.
pub fn spawn_process(spec: SpawnSpec) -> Result<Box<dyn ProcessHandle>, ProcessError> {
    current_spawner().spawn(spec)
}

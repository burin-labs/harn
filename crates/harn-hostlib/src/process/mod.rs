//! Process abstraction used by the deterministic process tools.
//!
//! Production code spawns through [`spawn_process`], which dispatches to
//! the [`ProcessSpawner`] currently installed via
//! [`install_spawner`]. The default spawner ([`real::default_spawner`])
//! goes through `harn_vm::process_sandbox`. Tests install
//! [`mock::MockSpawner`] to drive process behaviour deterministically.

pub mod handle;
pub mod mock;
pub mod owner_death;
pub mod real;

#[cfg(target_os = "windows")]
mod windows;

pub use handle::{
    current_spawner, install_spawner, spawn_process, EnvMode, ExitStatus, OutputCapture,
    OwnerDeathPolicy, ProcessCleanupChild, ProcessCleanupReport, ProcessError, ProcessHandle,
    ProcessKiller, ProcessSpawner, SpawnSpec, SpawnerGuard, WaitOutcome,
};
pub use mock::{MockHandleController, MockProcess, MockProcessConfig, MockSpawner};
pub use real::default_spawner;
#[cfg(unix)]
pub use real::replace_current_process;
#[cfg(target_os = "windows")]
pub use windows::KillOnCloseJob;

/// Shared by this crate's tests only.
#[cfg(test)]
pub(crate) mod test_support {
    /// Declares that a test's spawns inherit this process's environment.
    ///
    /// Since harn#8477 an inheriting spawn refuses when no session
    /// environment is installed, because absence used to read as permission.
    /// A test whose subject is spawning rather than credential scope installs
    /// this and keeps asserting what it is about.
    pub(crate) struct InheritedForTest;

    impl InheritedForTest {
        pub(crate) fn install() -> Self {
            harn_vm::stdlib::process::set_session_environment(Some(
                harn_vm::security::SessionEnvironment::inherited(),
            ));
            Self
        }
    }

    impl Drop for InheritedForTest {
        fn drop(&mut self) {
            harn_vm::stdlib::process::set_session_environment(None);
        }
    }
}

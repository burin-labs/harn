//! Process abstraction used by the deterministic process tools.
//!
//! Production code spawns through [`spawn_process`], which dispatches to
//! the [`ProcessSpawner`] currently installed via
//! [`install_spawner`]. The default spawner ([`real::default_spawner`])
//! goes through `harn_vm::process_sandbox`. Tests install
//! [`mock::MockSpawner`] to drive process behaviour deterministically.

pub mod handle;
pub mod mock;
pub mod real;

pub use handle::{
    current_spawner, install_spawner, spawn_process, EnvMode, ExitStatus, ProcessError,
    ProcessHandle, ProcessKiller, ProcessSpawner, SpawnSpec, SpawnerGuard,
};
pub use mock::{MockHandleController, MockProcess, MockProcessConfig, MockSpawner};
pub use real::default_spawner;

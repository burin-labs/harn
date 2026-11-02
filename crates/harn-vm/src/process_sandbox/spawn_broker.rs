//! Embedder hook that runs one exact argument-list spawn outside the active
//! process confinement.
//!
//! Some trusted host commands cannot work inside the confined child: a
//! read-only CLI invocation that needs host credentials the child's closed
//! environment and filesystem view deliberately hide. An embedder that has
//! validated such an invocation can install a [`SpawnBroker`]. The process
//! host consults it for every direct program spawn, after the universal
//! catastrophic-command floor has run and before any confinement is applied.
//! When the broker returns a [`SpawnBrokerGrant`], that exact argv runs
//! unconfined, in the grant's working directory, with the grant's environment
//! as the child's whole environment. The command receipt then reports
//! `sandbox.enforced: false` with the grant's `broker` id.
//!
//! The broker is never consulted for a shell interpreter, so a grant can only
//! ever name one program and its literal arguments.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Decides whether one direct program spawn may run outside confinement.
pub trait SpawnBroker: Send + Sync {
    /// Called only for argument-list spawns. Return `Some` to run exactly
    /// this argv outside the active confinement; `None` leaves the spawn on
    /// its normal confined path.
    fn admit(&self, request: &SpawnBrokerRequest<'_>) -> Option<SpawnBrokerGrant>;
}

/// The spawn as the caller requested it, before confinement.
#[derive(Clone, Copy, Debug)]
pub struct SpawnBrokerRequest<'a> {
    /// Program exactly as requested (not resolved against `PATH`).
    pub program: &'a str,
    /// Literal arguments.
    pub args: &'a [String],
    /// Requested working directory, if any.
    pub cwd: Option<&'a Path>,
    /// The caller's environment overrides. Informational only: none of them
    /// reach a brokered child unless the grant repeats them.
    pub env: &'a BTreeMap<String, String>,
}

/// Permission to run one spawn unconfined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnBrokerGrant {
    /// Stable id reported as `sandbox.broker` on the command receipt.
    pub broker_id: String,
    /// Working directory for the child. The workspace-root check is skipped.
    pub cwd: PathBuf,
    /// The child's COMPLETE environment. Nothing is inherited from the host
    /// process and the caller's overrides and removals are not applied. The
    /// process host adds only its private process-cleanup token, which it
    /// needs to reap the child's tree.
    pub env: BTreeMap<String, String>,
}

/// Returned when a broker is already installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a spawn broker is already installed for this process")]
pub struct SpawnBrokerAlreadyInstalled;

static SPAWN_BROKER: OnceLock<Arc<dyn SpawnBroker>> = OnceLock::new();

/// Install the process-wide spawn broker.
///
/// Settable once per process. A second install is refused and leaves the
/// first broker in place, so no later caller can silently widen or replace
/// what the embedder admitted.
pub fn install_spawn_broker(
    broker: Arc<dyn SpawnBroker>,
) -> Result<(), SpawnBrokerAlreadyInstalled> {
    SPAWN_BROKER
        .set(broker)
        .map_err(|_| SpawnBrokerAlreadyInstalled)
}

/// Ask the installed broker about one direct program spawn.
///
/// Returns `None` without consulting anything when no broker is installed or
/// when `program` is a shell interpreter.
pub fn admit_spawn(request: &SpawnBrokerRequest<'_>) -> Option<SpawnBrokerGrant> {
    let broker = SPAWN_BROKER.get()?;
    if is_shell_interpreter(request.program) {
        return None;
    }
    broker.admit(request)
}

const SHELL_INTERPRETERS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "mksh",
    "fish",
    "csh",
    "tcsh",
    "nu",
    "cmd",
    "powershell",
    "pwsh",
];

fn is_shell_interpreter(program: &str) -> bool {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let name = name.to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    SHELL_INTERPRETERS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::is_shell_interpreter;

    #[test]
    fn shells_are_recognized_by_name_and_by_path() {
        for shell in [
            "sh",
            "bash",
            "zsh",
            "dash",
            "fish",
            "cmd",
            "cmd.exe",
            "powershell",
            "PowerShell.exe",
            "pwsh",
            "/bin/sh",
            "/usr/local/bin/bash",
            "/opt/homebrew/bin/fish",
            r"C:\Windows\System32\cmd.exe",
        ] {
            assert!(is_shell_interpreter(shell), "{shell}");
        }
        for program in ["env", "/usr/bin/git", "shellcheck", "bashful", "cmdx"] {
            assert!(!is_shell_interpreter(program), "{program}");
        }
    }
}

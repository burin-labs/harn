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
//! Harn never brokers a shell interpreter or a Windows batch script, either as
//! requested or as the program the grant's `PATH` resolves. Every other argv,
//! including a launcher such as `env` or `xargs` that runs another program, is
//! the broker's decision: admit only argv whose every element you validated.

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
    /// Working directory for the child. The workspace-root check is skipped;
    /// the catastrophic-command floor is re-checked against it.
    pub cwd: PathBuf,
    /// The child's COMPLETE environment. Nothing is inherited from the host
    /// process and the caller's overrides and removals are not applied. The
    /// process host adds only its private process-tree tokens, which it needs
    /// to reap the child's tree. A bare program name resolves against this
    /// environment's `PATH` (and `PATHEXT` on Windows).
    pub env: BTreeMap<String, String>,
}

/// A grant Harn accepted, with the program resolved against the grant's
/// environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedSpawn {
    /// The broker's grant.
    pub grant: SpawnBrokerGrant,
    /// Program to execute: the request's program resolved against the grant's
    /// `PATH`, or the request's program unchanged when it does not resolve.
    pub program: String,
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
/// when `program` is a shell interpreter, and `None` after consulting when
/// the broker declines or the grant's `PATH` resolves `program` to a shell or
/// batch script.
pub fn admit_spawn(request: &SpawnBrokerRequest<'_>) -> Option<AdmittedSpawn> {
    let broker = SPAWN_BROKER.get()?;
    if runs_through_shell(request.program) {
        return None;
    }
    let grant = broker.admit(request)?;
    let overlay: Vec<(String, String)> = grant
        .env
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let program =
        crate::stdlib::process::resolve_program_path(request.program, &None, true, &overlay);
    if runs_through_shell(&program) {
        return None;
    }
    Some(AdmittedSpawn { grant, program })
}

const SHELL_INTERPRETERS: &[&str] = &[
    "sh",
    "ash",
    "bash",
    "rbash",
    "zsh",
    "dash",
    "ksh",
    "mksh",
    "yash",
    "fish",
    "csh",
    "tcsh",
    "nu",
    "elvish",
    "xonsh",
    "busybox",
    "cmd",
    "powershell",
    "pwsh",
];

/// A shell interpreter by name or path, or a Windows batch script, which
/// always runs through `cmd.exe`.
fn runs_through_shell(program: &str) -> bool {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let name = name.to_ascii_lowercase();
    if name.ends_with(".bat") || name.ends_with(".cmd") {
        return true;
    }
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    SHELL_INTERPRETERS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::runs_through_shell;

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
            "ash",
            "/bin/busybox",
            "rbash",
            "npm.cmd",
            r"C:\tools\build.BAT",
        ] {
            assert!(runs_through_shell(shell), "{shell}");
        }
        for program in ["env", "/usr/bin/git", "shellcheck", "bashful", "cmdx"] {
            assert!(!runs_through_shell(program), "{program}");
        }
    }
}

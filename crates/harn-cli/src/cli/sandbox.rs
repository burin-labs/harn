//! The sandbox and capability flag block shared by every command that launches
//! a run.
//!
//! `harn run` and `harn time run` execute the *same* script under the *same*
//! runtime; they differ only in what they report afterwards. Their confinement
//! surface must therefore be identical, and the way to guarantee that is for
//! there to be one declaration of it. Before this struct existed the two
//! commands each hand-declared the six sandbox flags, and `harn time run`
//! silently lacked the capability-profile flags that `harn run` had gained —
//! the predictable outcome of a duplicated surface, not an oversight anyone
//! could have caught by reading either file alone.
//!
//! Flatten this into a command's args with `#[command(flatten)]` and build the
//! run options with [`crate::commands::run::sandbox_options_from_args`]. A
//! new confinement flag then lands in one place and every launcher gets it.

use clap::Args;
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Args)]
pub(crate) struct SandboxArgs {
    /// Disable the default worktree filesystem/process sandbox and
    /// network egress fail-closed guard for this run.
    #[arg(long = "no-sandbox", action = clap::ArgAction::SetTrue)]
    pub no_sandbox: bool,
    /// Permit commands spawned by this run to open network sockets while
    /// retaining the worktree filesystem and process sandbox.
    #[arg(
        long = "allow-process-network",
        action = clap::ArgAction::SetTrue,
        conflicts_with = "no_sandbox"
    )]
    pub allow_process_network: bool,
    /// Extra read-only filesystem roots. Repeatable; each path is
    /// readable but never writable.
    #[arg(
        long = "read-only-root",
        value_name = "PATH",
        conflicts_with = "no_sandbox"
    )]
    pub read_only_root: Vec<PathBuf>,
    /// Extra writable filesystem roots. Repeatable; each path becomes
    /// part of the run's write jail while sandboxing stays enabled.
    #[arg(
        long = "write-root",
        visible_alias = "writable-root",
        value_name = "PATH",
        conflicts_with = "no_sandbox"
    )]
    pub write_root: Vec<PathBuf>,
    /// Extra subprocess-only read roots. Repeatable; Harn filesystem builtins
    /// do not gain access to these paths.
    #[arg(
        long = "sandbox-read-root",
        value_name = "PATH",
        conflicts_with = "no_sandbox"
    )]
    pub sandbox_read_root: Vec<PathBuf>,
    /// Extra subprocess-only write roots. Repeatable; Harn filesystem builtins
    /// do not gain access to these paths.
    #[arg(
        long = "sandbox-write-root",
        value_name = "PATH",
        conflicts_with = "no_sandbox"
    )]
    pub sandbox_write_root: Vec<PathBuf>,
    /// Session credential posture: `hermetic` admits no credentials at all;
    /// `lane` carries the declared `--grant` set. Absent, the run inherits the
    /// launcher environment unchanged (the legacy path). See `--grant`.
    #[arg(
        long = "capability-profile",
        value_enum,
        value_name = "PROFILE",
        conflicts_with = "no_sandbox"
    )]
    pub capability_profile: Option<crate::commands::run::CapabilityProfileArg>,
    /// Grant one named credential to this session. Repeatable.
    ///
    /// `NAME=SOURCE[,expose=ENV_VAR]`, where `SOURCE` is `env:VAR_NAME` (a
    /// launcher variable, snapshotted at launch) or `secret://ACCOUNT/KEY` (a
    /// secret-store pointer). The optional `,expose=ENV_VAR` publishes the
    /// value as `ENV_VAR` to spawned commands and to this run's own model
    /// calls. `provider:PROVIDER` is a shorthand that takes the credential
    /// variable from the provider catalog. Any `--grant` selects the `lane`
    /// profile; nothing else from the launcher environment crosses the
    /// boundary. For example
    /// `--grant gh_token=secret://gh/token,expose=GH_TOKEN` or
    /// `--grant provider:fireworks`.
    #[arg(long = "grant", value_name = "SPEC", conflicts_with = "no_sandbox")]
    pub grant: Vec<String>,
}

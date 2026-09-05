use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct UpgradeArgs {
    /// Pin to a specific release tag (e.g. v0.8.19). Defaults to the
    /// latest published GitHub release.
    #[arg(long, value_name = "TAG")]
    pub version: Option<String>,

    /// Print the resolved target version and exit without downloading.
    #[arg(long)]
    pub check: bool,

    /// Skip SHA256SUMS verification of the downloaded archive.
    #[arg(long)]
    pub no_verify: bool,

    /// Reinstall even when the installed version already matches the
    /// resolved target.
    #[arg(long)]
    pub force: bool,

    /// Emit a structured `JsonEnvelope` instead of human-readable output. Pairs
    /// naturally with `--check` for an agent-readable upgrade-readiness probe;
    /// when combined with a full install, the envelope is printed after the
    /// install action completes so callers can read the final status.
    /// See `docs/src/cli-json-contract.md` for the envelope shape.
    #[arg(long)]
    pub json: bool,

    /// Install a locally supplied release archive without fetching or changing hook enrollment.
    #[arg(long, requires_all = ["archive_sha256", "install_dir", "version"], conflicts_with_all = ["check", "no_verify"])]
    pub archive: Option<std::path::PathBuf>,

    /// Required SHA256 of the local archive, verified before extraction.
    #[arg(long, requires = "archive")]
    pub archive_sha256: Option<String>,

    /// Destination for the verified local archive installation.
    #[arg(long, requires = "archive")]
    pub install_dir: Option<std::path::PathBuf>,
}

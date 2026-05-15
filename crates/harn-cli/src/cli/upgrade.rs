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
}

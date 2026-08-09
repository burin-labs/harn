use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct WatchArgs {
    /// Deny specific builtins as a comma-separated list.
    #[arg(long, conflicts_with = "allow")]
    pub deny: Option<String>,
    /// Allow only the listed builtins as a comma-separated list.
    #[arg(long, conflicts_with = "deny")]
    pub allow: Option<String>,
    /// Path to the .harn file to watch.
    pub file: String,
}

use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Args)]
pub(crate) struct RepoArgs {
    #[command(subcommand)]
    pub command: RepoCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RepoCommand {
    /// Count maintained source using the repository's ownership and exclusion registry.
    Loc {
        #[arg(default_value = ".")]
        directory: PathBuf,
        /// Registry JSON; defaults to <directory>/scripts/repo-loc.json.
        #[arg(long)]
        registry: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
}

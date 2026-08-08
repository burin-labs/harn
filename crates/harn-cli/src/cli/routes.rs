use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
pub(crate) struct RoutesArgs {
    /// Project root, harn.toml path, or file inside the project.
    pub root: PathBuf,

    /// Emit a stable JSON envelope instead of the text table.
    #[arg(long)]
    pub json: bool,
}

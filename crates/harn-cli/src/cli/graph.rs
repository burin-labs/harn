use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
pub(crate) struct GraphArgs {
    /// Harn file or directory to analyze.
    pub root: PathBuf,

    /// Emit a stable JSON envelope instead of the text tree.
    #[arg(long)]
    pub json: bool,

    /// Focus output on one module path, import path, or basename.
    #[arg(long, value_name = "MODULE")]
    pub module: Option<String>,
}

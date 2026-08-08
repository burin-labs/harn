use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
pub(crate) struct DocArgs {
    /// Harn file or project directory to document. Defaults to the current
    /// directory. Every `.harn` file under a directory is scanned for `pub`
    /// symbols; nested `target`, `.git`, `.harn`, and `node_modules` trees
    /// are skipped.
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Write the rendered Markdown to this file instead of stdout.
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,
}

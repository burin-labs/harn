use std::path::PathBuf;

use clap::Args;

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct PrecompileArgs {
    /// File or directory to precompile. Directories are walked recursively
    /// for `.harn` files.
    pub target: PathBuf,
    /// Output directory for compiled `.harnbc` artifacts. When omitted,
    /// each artifact is written next to its source. The directory tree
    /// under `target` is mirrored under `--out` so per-source pathing is
    /// preserved.
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,
    /// Continue processing the remaining sources even if one fails to
    /// compile. The exit code still reflects the failure.
    #[arg(long)]
    pub keep_going: bool,
    /// Suppress per-file progress output.
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

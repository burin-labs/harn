use std::path::PathBuf;

use clap::Args;

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct PrecompileArgs {
    /// File or directory to precompile. Directories are walked recursively
    /// for `.harn` files.
    #[arg(required_unless_present = "artifact_contract")]
    pub target: Option<PathBuf>,
    /// Print the machine-readable adjacent-artifact compatibility contract.
    #[arg(
        long,
        conflicts_with_all = ["target", "relocatable", "out", "keep_going", "quiet"]
    )]
    pub artifact_contract: bool,
    /// Use a path-independent import-graph key for an artifact that moves
    /// with its complete source tree. Used by the directory-walk child.
    #[arg(long, hide = true)]
    pub relocatable: bool,
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

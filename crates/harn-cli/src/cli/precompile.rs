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
        conflicts_with_all = ["target", "relocatable", "out", "keep_going", "quiet", "jobs"]
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
    /// Compile up to N sources concurrently when walking a directory.
    /// Defaults to the machine's available parallelism. Output order,
    /// artifacts, and the summary are the same for every value.
    #[arg(
        long = "jobs",
        short = 'j',
        value_name = "N",
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub jobs: Option<u32>,
    /// Suppress per-file progress output.
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

/// Env var carrying the resolved `--jobs` bound to the embedded driver script.
pub const PRECOMPILE_JOBS_ENV: &str = "HARN_PRECOMPILE_JOBS";

impl PrecompileArgs {
    /// The concurrency bound the directory walk runs with. Resolved in Rust so
    /// the driver always receives a concrete bound: the script has no portable
    /// way to ask for the core count.
    pub fn resolved_jobs(&self) -> usize {
        self.jobs.map_or_else(
            || {
                std::thread::available_parallelism()
                    .map(std::num::NonZeroUsize::get)
                    .unwrap_or(1)
            },
            |jobs| jobs as usize,
        )
    }
}

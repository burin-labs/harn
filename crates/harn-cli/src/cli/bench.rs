use std::path::PathBuf;

use clap::{Args, Subcommand};

use super::ProfileArgs;

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub(crate) struct BenchArgs {
    #[command(subcommand)]
    pub command: Option<BenchCommand>,
    /// Path to the .harn file to benchmark.
    pub file: Option<String>,
    /// Number of benchmark iterations to run.
    #[arg(short = 'n', long, default_value_t = 10)]
    pub iterations: usize,
    #[command(flatten)]
    pub profile: ProfileArgs,
}

#[derive(Debug, Subcommand)]
pub(crate) enum BenchCommand {
    /// Score deterministic replay fixtures and emit a leaderboard-ready report.
    Replay(BenchReplayArgs),
}

#[derive(Debug, Args)]
pub(crate) struct BenchReplayArgs {
    /// Replay benchmark suite manifest, fixture file, or fixture directory.
    /// Defaults to benchmarks/replay/suite.json.
    pub selection: Option<PathBuf>,
    /// Emit the machine-readable report to stdout.
    #[arg(long)]
    pub json: bool,
    /// Write the machine-readable report to this path.
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,
    /// Run only fixtures whose path or trace name contains this string.
    #[arg(long, value_name = "TEXT")]
    pub filter: Option<String>,
    /// Suite name to place in the benchmark report.
    #[arg(
        long = "suite-name",
        default_value = "harn-canonical-replay-determinism"
    )]
    pub suite_name: String,
    /// External trace adapter to use for --external-first/--external-second.
    #[arg(long, value_name = "ADAPTER", requires_all = ["external_first", "external_second"])]
    pub adapter: Option<String>,
    /// First external trace file for adapter-based comparison.
    #[arg(long = "external-first", value_name = "PATH", requires = "adapter")]
    pub external_first: Option<PathBuf>,
    /// Second external trace file for adapter-based comparison.
    #[arg(long = "external-second", value_name = "PATH", requires = "adapter")]
    pub external_second: Option<PathBuf>,
    /// Name for the adapted external trace pair.
    #[arg(long = "external-name", default_value = "external-adapted-replay")]
    pub external_name: String,
}

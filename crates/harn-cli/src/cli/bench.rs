use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use harn_kernel::{
    PORTABLE_MAX_COMPILE_ITERATIONS, PORTABLE_MAX_DISPATCH_ITERATIONS, PORTABLE_MAX_WORKERS,
};

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
    /// Benchmark canonical Portable Kernel compile, decode, and dispatch paths.
    Portable(BenchPortableArgs),
    /// Score deterministic replay fixtures and emit a leaderboard-ready report.
    Replay(BenchReplayArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum PortableEntryKindArg {
    Function,
    Pipeline,
}

#[derive(Debug, Args)]
pub(crate) struct BenchPortableArgs {
    /// Harn source file containing the portable entry.
    pub source: PathBuf,
    /// Function or pipeline name to compile.
    #[arg(long, default_value = "main")]
    pub entry: String,
    /// Kind of callable named by --entry.
    #[arg(long = "entry-kind", value_enum, default_value_t = PortableEntryKindArg::Function)]
    pub entry_kind: PortableEntryKindArg,
    /// JSON file passed as the entry's input value.
    #[arg(long, value_name = "PATH")]
    pub input: PathBuf,
    /// Number of isolated dispatches to measure (1..=1,000,000).
    #[arg(
        short = 'n',
        long,
        default_value_t = 500,
        value_parser = clap::builder::RangedU64ValueParser::<usize>::new()
            .range(1..=PORTABLE_MAX_DISPATCH_ITERATIONS as u64)
    )]
    pub iterations: usize,
    /// Number of native operating-system workers (1..=256).
    #[arg(
        long,
        default_value_t = 1,
        value_parser = clap::builder::RangedU64ValueParser::<usize>::new()
            .range(1..=PORTABLE_MAX_WORKERS as u64)
    )]
    pub threads: usize,
    /// Number of repeated compile and artifact-decode samples (1..=100,000).
    #[arg(
        long = "compile-iterations",
        default_value_t = 30,
        value_parser = clap::builder::RangedU64ValueParser::<usize>::new()
            .range(1..=PORTABLE_MAX_COMPILE_ITERATIONS as u64)
    )]
    pub compile_iterations: usize,
    /// Emit only the machine-readable benchmark receipt.
    #[arg(long)]
    pub json: bool,
    /// Write the benchmark receipt as JSON.
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct BenchReplayArgs {
    /// Replay benchmark suite manifest, fixture file, or fixture directory.
    /// Defaults to bench/replay/suite.json.
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

use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct BenchArgs {
    /// Path to the .harn file to benchmark.
    pub file: String,
    /// Number of benchmark iterations to run.
    #[arg(short = 'n', long, default_value_t = 10)]
    pub iterations: usize,
}

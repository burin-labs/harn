use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct VizArgs {
    /// Path to the .harn file to visualize.
    pub file: String,
    /// Optional output path. Defaults to stdout.
    #[arg(short, long)]
    pub output: Option<String>,
}

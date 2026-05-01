use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct RunsArgs {
    #[command(subcommand)]
    pub command: RunsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RunsCommand {
    /// Inspect a persisted run record and optionally diff it against another.
    Inspect(RunsInspectArgs),
}

#[derive(Debug, Args)]
pub(crate) struct RunsInspectArgs {
    /// Path to the run record JSON file.
    pub path: String,
    /// Optional baseline run record to diff against.
    #[arg(long)]
    pub compare: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ReplayArgs {
    /// Path to the run record JSON file.
    pub path: String,
}

#[derive(Debug, Args)]
pub(crate) struct EvalArgs {
    /// Run record path, run directory, or eval manifest path.
    pub path: String,
    /// Optional baseline run record for diffing.
    #[arg(long)]
    pub compare: Option<String>,
    /// Run a pipeline twice and compare the baseline against this structural experiment.
    #[arg(long = "structural-experiment")]
    pub structural_experiment: Option<String>,
    /// Replay LLM responses from a JSONL fixture file when `path` is a `.harn` pipeline.
    #[arg(
        long = "llm-mock",
        value_name = "PATH",
        conflicts_with = "llm_mock_record"
    )]
    pub llm_mock: Option<String>,
    /// Record executed LLM responses into a JSONL fixture file when `path` is a `.harn` pipeline.
    #[arg(
        long = "llm-mock-record",
        value_name = "PATH",
        conflicts_with = "llm_mock"
    )]
    pub llm_mock_record: Option<String>,
    /// Positional arguments forwarded to `harn run <pipeline.harn> -- ...` when
    /// `path` is a pipeline file and `--structural-experiment` is set.
    #[arg(last = true)]
    pub argv: Vec<String>,
}

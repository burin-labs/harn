use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct TraceArgs {
    #[command(subcommand)]
    pub command: TraceCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TraceCommand {
    /// Convert a generic JSONL trace into a replayable `--llm-mock` fixture.
    Import(TraceImportArgs),
}

#[derive(Debug, Args)]
pub(crate) struct TraceImportArgs {
    /// Source JSONL trace file with `{prompt,response,tool_calls}` records.
    #[arg(long = "trace-file", value_name = "PATH")]
    pub trace_file: String,
    /// Optional trace id to filter when the source file contains multiple traces.
    #[arg(long = "trace-id", value_name = "ID")]
    pub trace_id: Option<String>,
    /// Output path for the generated replay fixture JSONL.
    #[arg(long = "output", value_name = "PATH")]
    pub output: String,
}

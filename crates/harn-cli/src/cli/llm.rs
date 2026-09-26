use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Args)]
pub(crate) struct LlmArgs {
    #[command(subcommand)]
    pub command: LlmCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum LlmCommand {
    /// Answer typed questions with one bounded evaluation and a complete receipt.
    Evaluate(LlmEvaluateArgs),
}

#[derive(Debug, Args)]
pub(crate) struct LlmEvaluateArgs {
    /// JSON object with site_id, state, questions and policy. Labels are not accepted.
    #[arg(long, conflicts_with_all = ["model", "state_file", "questions", "policy"])]
    pub request: Option<PathBuf>,
    /// Verify a saved receipt against --request without credentials or dispatch.
    #[arg(long, requires = "request")]
    pub verify_receipt: Option<PathBuf>,
    /// Record to a new evaluation tape, or strictly replay an existing tape offline.
    #[arg(long, conflicts_with = "verify_receipt")]
    pub tape: Option<PathBuf>,
    /// Catalog model route, for example openrouter/typesafe/jev-1.13.
    #[arg(long, required_unless_present = "request")]
    pub model: Option<String>,
    /// State as JSON, or plain text if the file is not JSON.
    #[arg(long, required_unless_present = "request")]
    pub state_file: Option<PathBuf>,
    /// JSON object mapping question IDs to std/predicate question records.
    #[arg(long, required_unless_present = "request")]
    pub questions: Option<PathBuf>,
    /// Complete EvaluationPolicy JSON; defaults to native, threshold 0.5, $0.01 ceilings.
    #[arg(long)]
    pub policy: Option<PathBuf>,
    /// Stable caller identity included in the receipt.
    #[arg(long, default_value = "cli.evaluate.v1")]
    pub site_id: String,
    /// Emit the complete EvaluationResult as JSON.
    #[arg(long)]
    pub json: bool,
}

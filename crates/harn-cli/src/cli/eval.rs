//! Clap definitions for `harn eval` and its subcommands.
//!
//! The bare form `harn eval <path>` evaluates a run record, run directory,
//! eval manifest, or `.harn` pipeline (legacy entrypoint, dispatched through
//! `eval_run_record`). The `harn eval prompt <file> --fleet <models>`
//! subcommand renders (and optionally runs / judges) a single
//! `.harn.prompt` template against a fleet of models so authors can compare
//! the wire envelope each capability profile materializes.

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct EvalArgs {
    /// Run record path, run directory, eval manifest path, or `.harn` pipeline.
    /// Required unless a subcommand (e.g. `prompt`) is used.
    pub path: Option<String>,
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
    #[command(subcommand)]
    pub command: Option<EvalCommand>,
}

#[derive(Debug, Subcommand)]
pub enum EvalCommand {
    /// Render and optionally run a `.harn.prompt` across a fleet of models.
    Prompt(EvalPromptArgs),
}

#[derive(Debug, Args)]
pub struct EvalPromptArgs {
    /// Path to a `.harn.prompt` (or `.prompt`) template.
    pub file: PathBuf,
    /// Fleet of model selectors (comma-separated, repeatable).
    /// Each entry is either a model alias (`claude-opus-4-7`) or a
    /// `provider:model` selector (`ollama:qwen3.5`). Mutually exclusive
    /// with `--fleet-name`.
    #[arg(
        long,
        value_delimiter = ',',
        required_unless_present = "fleet_name",
        conflicts_with = "fleet_name"
    )]
    pub fleet: Vec<String>,
    /// Named fleet from `harn.toml` `[eval.fleets.<name>]`.
    #[arg(long = "fleet-name")]
    pub fleet_name: Option<String>,
    /// JSON file with bindings injected into the template scope.
    #[arg(long)]
    pub bindings: Option<PathBuf>,
    /// Evaluation mode.
    #[arg(long, value_enum, default_value_t = EvalPromptMode::Render)]
    pub mode: EvalPromptMode,
    /// Output format.
    #[arg(long, value_enum, default_value_t = EvalPromptOutput::Terminal)]
    pub output: EvalPromptOutput,
    /// Output destination for HTML / JSON (defaults to stdout).
    #[arg(long = "out-file", short = 'o')]
    pub out_file: Option<PathBuf>,
    /// Maximum concurrent model invocations in run/judge modes.
    #[arg(long, default_value_t = 4)]
    pub max_concurrent: usize,
    /// Optional judge prompt template. When unset, a built-in equivalence
    /// judge is used.
    #[arg(long = "judge-template")]
    pub judge_template: Option<PathBuf>,
    /// Model used for `--mode judge` evaluation.
    #[arg(long = "judge-model", default_value = "claude-opus-4-7")]
    pub judge_model: String,
    /// Maximum tokens for `--mode run` / `--mode judge` calls.
    #[arg(long = "max-tokens", default_value_t = 1024)]
    pub max_tokens: i64,
    /// Treat unauthenticated providers as errors rather than skipping them.
    #[arg(long = "fail-on-unauthorized")]
    pub fail_on_unauthorized: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum EvalPromptMode {
    /// Render the template against each model's capability profile.
    Render,
    /// Render + execute against each model and collect outputs.
    Run,
    /// Render + run + LLM-as-judge equivalence scoring.
    Judge,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum EvalPromptOutput {
    Terminal,
    Json,
    Html,
}

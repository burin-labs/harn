use std::path::PathBuf;

use clap::{Args, Subcommand};

use super::util::llm_model_completion_parser;

/// `harn local` — manage local LLM runtimes (Ollama, llama.cpp,
/// MLX, generic OpenAI-compatible servers) through one stable
/// abstraction while underlying CLIs keep changing.
#[derive(Debug, Args)]
pub(crate) struct LocalArgs {
    #[command(subcommand)]
    pub command: LocalCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum LocalCommand {
    /// Survey every local provider Harn knows about: base URL, reachability,
    /// served models, loaded models, memory footprint, context, keep-alive.
    List(LocalListArgs),
    /// Show the currently-selected local provider/model and a brief summary
    /// of every other local runtime.
    Status(LocalStatusArgs),
    /// Make `<alias>` the active local model: warm it on its provider,
    /// unload conflicting models, and persist the selection.
    Switch(LocalSwitchArgs),
    /// Explain the selected local runtime profile and required probes.
    Profile(LocalProfileArgs),
    /// Unload loaded local models. By default targets the active provider;
    /// pass `--all` to unload every reachable local provider.
    Stop(LocalStopArgs),
}

#[derive(Debug, Args)]
pub(crate) struct LocalListArgs {
    /// Emit a structured JSON snapshot instead of a human table.
    #[arg(long)]
    pub json: bool,
    /// Restrict to one provider id (e.g. `ollama`, `llamacpp`, `mlx`).
    #[arg(long)]
    pub provider: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct LocalStatusArgs {
    /// Emit a structured JSON snapshot instead of human text.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct LocalSwitchArgs {
    /// Model alias or provider-native model id (e.g. `qwen36-coder`,
    /// `ollama:llama3.2`, `mlx-qwen36-27b`).
    #[arg(
        value_parser = llm_model_completion_parser(),
        hide_possible_values = true
    )]
    pub model: String,
    /// Override the inferred provider (e.g. force `--provider llamacpp` for
    /// a GGUF id that would otherwise route to `ollama`).
    #[arg(long)]
    pub provider: Option<String>,
    /// Context window override (Ollama: `num_ctx`). Defaults come from the
    /// machine profile derived from `harn models recommend`.
    #[arg(long)]
    pub ctx: Option<u64>,
    /// Keep-alive value to apply on the target provider (Ollama only at the
    /// moment; e.g. `30m`, `forever`, `-1`).
    #[arg(long = "keep-alive")]
    pub keep_alive: Option<String>,
    /// Skip pulling the model when it is missing (Ollama only).
    #[arg(long = "no-pull")]
    pub no_pull: bool,
    /// Skip unloading other local providers / sibling models.
    #[arg(long = "no-evict")]
    pub no_evict: bool,
    /// Allow an experimental or quarantined runtime without passing the
    /// profile's required probes.
    #[arg(long)]
    pub force: bool,
    /// JSON output from `harn provider-tool-probe`; can satisfy the
    /// profile's `tool_probe` requirement.
    #[arg(long = "probe-result")]
    pub probe_results: Vec<PathBuf>,
    /// Mark an externally-run probe as passed, for example
    /// `--passed-probe two_turn_cache_probe`.
    #[arg(long = "passed-probe")]
    pub passed_probes: Vec<String>,
    /// Emit a structured JSON result.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct LocalProfileArgs {
    /// Model alias or provider-native model id.
    #[arg(
        value_parser = llm_model_completion_parser(),
        hide_possible_values = true
    )]
    pub model: String,
    /// Override the inferred provider/runtime.
    #[arg(long)]
    pub provider: Option<String>,
    /// Emit a structured JSON result.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct LocalStopArgs {
    /// Unload every reachable local provider, not just the active one.
    #[arg(long)]
    pub all: bool,
    /// Target one provider id (overrides `--all`).
    #[arg(long)]
    pub provider: Option<String>,
    /// Emit a structured JSON result.
    #[arg(long)]
    pub json: bool,
}

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
    /// Launch a Harn-managed local server process and verify it is ready.
    Launch(Box<LocalLaunchArgs>),
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
pub(crate) struct LocalLaunchArgs {
    /// Model alias or provider-native model id to serve.
    #[arg(
        value_parser = llm_model_completion_parser(),
        hide_possible_values = true
    )]
    pub model: String,
    /// Local provider runtime to launch or warm (`ollama`, `llamacpp`, `mlx`).
    #[arg(long)]
    pub provider: Option<String>,
    /// Local model file, directory, or Hugging Face repo id for launched servers.
    #[arg(long = "model-source", alias = "model-path")]
    pub model_source: Option<String>,
    /// Server command to execute.
    #[arg(long = "server-command")]
    pub server_command: Option<String>,
    /// Host interface for the launched server. Defaults to the provider base URL host.
    #[arg(long)]
    pub host: Option<String>,
    /// Port for the launched server. Defaults to the provider catalog base URL.
    #[arg(long)]
    pub port: Option<u16>,
    /// Context window to request from the runtime.
    #[arg(long)]
    pub ctx: Option<u64>,
    /// Keep-alive value for Ollama warmup (e.g. `30m`, `forever`, `-1`).
    #[arg(long = "keep-alive")]
    pub keep_alive: Option<String>,
    /// Skip pulling the model when it is missing (Ollama only).
    #[arg(long = "no-pull")]
    pub no_pull: bool,
    /// Number of parallel slots.
    #[arg(long, default_value_t = 1)]
    pub parallel: u64,
    /// llama.cpp GPU layer setting (`auto`, `all`, or a number).
    #[arg(long = "gpu-layers", default_value = "auto")]
    pub gpu_layers: String,
    /// llama.cpp K-cache type.
    #[arg(long = "cache-type-k")]
    pub cache_type_k: Option<String>,
    /// llama.cpp V-cache type.
    #[arg(long = "cache-type-v")]
    pub cache_type_v: Option<String>,
    /// llama.cpp prompt/KV cache RAM MiB cap.
    #[arg(long = "cache-ram")]
    pub cache_ram: Option<u64>,
    /// llama.cpp reasoning mode (`on`, `off`, or `auto`).
    #[arg(long)]
    pub reasoning: Option<String>,
    /// llama.cpp reasoning extraction format, for example `deepseek`.
    #[arg(long = "reasoning-format")]
    pub reasoning_format: Option<String>,
    /// llama.cpp flash-attention mode (`on`, `off`, or `auto`).
    #[arg(long = "flash-attn")]
    pub flash_attn: Option<String>,
    /// Enable the llama.cpp Jinja chat template parser.
    #[arg(long)]
    pub jinja: bool,
    /// Enable the llama.cpp Prometheus metrics endpoint.
    #[arg(long)]
    pub metrics: bool,
    /// Extra argument to pass through to the server command. Repeat as needed.
    #[arg(long = "server-arg", allow_hyphen_values = true)]
    pub server_args: Vec<String>,
    /// Readiness timeout in seconds.
    #[arg(long = "timeout-secs", default_value_t = 120)]
    pub timeout_secs: u64,
    /// Log file path. Defaults under Harn local state.
    #[arg(long)]
    pub log: Option<PathBuf>,
    /// Skip unloading other local providers / sibling models before launch.
    #[arg(long = "no-evict")]
    pub no_evict: bool,
    /// Allow a launch even when catalog memory estimates exceed current RAM headroom.
    #[arg(long = "allow-memory-risk")]
    pub allow_memory_risk: bool,
    /// Emit a structured JSON result.
    #[arg(long)]
    pub json: bool,
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

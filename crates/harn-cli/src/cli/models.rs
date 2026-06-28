use clap::{Args, Subcommand};

use super::util::llm_model_completion_parser;

#[derive(Debug, Args)]
pub(crate) struct ModelsArgs {
    #[command(subcommand)]
    pub command: ModelsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ModelsCommand {
    /// Print resolved metadata for a model alias or model id as JSON.
    Info(ModelInfoArgs),
    /// List models grouped by provider.
    List(ModelsListArgs),
    /// Pull an Ollama model or print setup steps for a known local runtime.
    Install(ModelsInstallArgs),
    /// Recommend a starter model for the current machine and credentials.
    Recommend(ModelRecommendArgs),
    /// Round-trip a small prompt through a model and report timing, tokens, and cost.
    Test(ModelsTestArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ModelInfoArgs {
    /// Verify provider-local readiness for the resolved model when supported.
    #[arg(long)]
    pub verify: bool,
    /// Warm/preload the resolved model when supported. Implies --verify.
    #[arg(long)]
    pub warm: bool,
    /// Ollama keep_alive value to use with --warm (for example 30m, forever, or -1).
    #[arg(long = "keep-alive", value_name = "VALUE")]
    pub keep_alive: Option<String>,
    /// Model alias or provider-native model id.
    #[arg(
        value_parser = llm_model_completion_parser(),
        hide_possible_values = true
    )]
    pub model: String,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsListArgs {
    /// Restrict to a single provider.
    #[arg(long)]
    pub provider: Option<String>,
    /// Emit JSON instead of a human table.
    #[arg(long)]
    pub json: bool,
    /// Only show locally-installed (Ollama) models.
    #[arg(long = "installed-only")]
    pub installed_only: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsInstallArgs {
    /// Model alias or provider-native id to install or set up.
    pub model: String,
    /// Skip the size-confirmation prompt.
    #[arg(long)]
    pub yes: bool,
    /// Optional Ollama keep-alive hint (e.g. `5m`, `1h`).
    #[arg(long = "keep-alive", value_name = "VALUE")]
    pub keep_alive: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ModelRecommendArgs {
    /// Emit the recommendation and hardware snapshot as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsTestArgs {
    /// Model alias or provider-native model id.
    pub model: String,
    /// Prompt text to send to the model.
    #[arg(long, default_value = "Reply with the word pong.")]
    pub prompt: String,
    /// Provider id to use instead of inferring one from the model selector.
    #[arg(long)]
    pub provider: Option<String>,
    /// Emit a structured JSON result.
    #[arg(long, default_value_t = false)]
    pub json: bool,
}

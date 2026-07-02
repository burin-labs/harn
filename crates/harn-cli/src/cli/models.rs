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
    /// Inspect LoRA adapter metadata and compatibility with a Harn model route.
    Lora(ModelsLoraArgs),
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
pub(crate) struct ModelsLoraArgs {
    #[command(subcommand)]
    pub command: ModelsLoraCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ModelsLoraCommand {
    /// Inspect a PEFT LoRA adapter directory or repo id.
    Inspect(ModelsLoraInspectArgs),
    /// Plan a portable LoRA/QLoRA tool-calling fine-tune for a Harn model route.
    Plan(ModelsLoraPlanArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ModelsLoraInspectArgs {
    /// Base model alias or provider-native id the adapter will attach to.
    #[arg(long = "base", value_parser = llm_model_completion_parser(), hide_possible_values = true)]
    pub base_model: String,
    /// Adapter directory or Hugging Face repo id.
    pub adapter: String,
    /// Request model name to expose for the adapter. Defaults to the adapter directory/repo basename.
    #[arg(long)]
    pub name: Option<String>,
    /// Provider/runtime to check against instead of inferring from the base model.
    #[arg(long)]
    pub provider: Option<String>,
    /// Emit structured JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelsLoraPlanArgs {
    /// Base model alias or provider-native id to fine-tune.
    #[arg(long = "base", value_parser = llm_model_completion_parser(), hide_possible_values = true)]
    pub base_model: String,
    /// Provider/runtime to plan against instead of inferring from the base model.
    #[arg(long)]
    pub provider: Option<String>,
    /// Tool-call format to train for (`auto`, `native`, `text`, or `json`).
    #[arg(long = "tool-format", default_value = "auto")]
    pub tool_format: String,
    /// Corpus path or dataset id to include in the generated eval/training recipe.
    #[arg(long, value_name = "PATH_OR_DATASET")]
    pub corpus: Option<String>,
    /// Adapter training method (`qlora` or `lora`).
    #[arg(long, default_value = "qlora")]
    pub method: String,
    /// Emit structured JSON.
    #[arg(long)]
    pub json: bool,
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

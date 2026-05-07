use clap::{ArgAction, Args};

use super::util::{llm_model_completion_parser, llm_provider_completion_parser};

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
pub(crate) struct ProviderCatalogArgs {
    /// Only include providers that are usable in the current environment.
    #[arg(long)]
    pub available_only: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ProviderReadyArgs {
    /// Provider id from Harn provider config, for example mlx or local.
    #[arg(
        value_parser = llm_provider_completion_parser(),
        hide_possible_values = true
    )]
    pub provider: String,
    /// Model alias or provider-native model id to require in /models.
    #[arg(
        long,
        value_parser = llm_model_completion_parser(),
        hide_possible_values = true
    )]
    pub model: Option<String>,
    /// Override the configured provider base URL for this probe.
    #[arg(long = "base-url")]
    pub base_url: Option<String>,
    /// Emit the full structured readiness result as JSON.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub json: bool,
}

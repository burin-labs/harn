use clap::{ArgAction, Args};

#[derive(Debug, Args)]
pub(crate) struct QuickstartArgs {
    /// Run with deterministic defaults and do not prompt.
    #[arg(long = "non-interactive", default_value_t = false, action = ArgAction::SetTrue)]
    pub non_interactive: bool,
    /// Provider to configure, for example anthropic, openai, or ollama.
    #[arg(long)]
    pub provider: Option<String>,
    /// Default model or model alias to write into starter config.
    #[arg(long)]
    pub model: Option<String>,
}

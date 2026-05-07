use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct ModelsArgs {
    #[command(subcommand)]
    pub command: ModelsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ModelsCommand {
    /// List models grouped by provider.
    List(ModelsListArgs),
    /// Pull a model via Ollama.
    Install(ModelsInstallArgs),
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
    /// Model id to pull (e.g. `llama3.2`, `qwen2.5:7b`).
    pub model: String,
    /// Skip the size-confirmation prompt.
    #[arg(long)]
    pub yes: bool,
    /// Optional Ollama keep-alive hint (e.g. `5m`, `1h`).
    #[arg(long = "keep-alive", value_name = "VALUE")]
    pub keep_alive: Option<String>,
}

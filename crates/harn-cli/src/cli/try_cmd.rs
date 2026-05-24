use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct TryArgs {
    /// Prompt to send to the agent loop.
    pub prompt: String,
    /// Maximum agent iterations (default 5).
    #[arg(long, default_value_t = 5)]
    pub max_iterations: u32,
    /// Force the agent tool format (`native`, `text`, or `auto`).
    #[arg(long = "tool-format")]
    pub tool_format: Option<String>,
    /// Reason for intentionally overriding the catalog-recommended tool format.
    #[arg(long = "override-reason")]
    pub override_reason: Option<String>,
}

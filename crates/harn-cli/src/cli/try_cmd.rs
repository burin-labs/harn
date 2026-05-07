use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct TryArgs {
    /// Prompt to send to the agent loop.
    pub prompt: String,
    /// Maximum agent iterations (default 5).
    #[arg(long, default_value_t = 5)]
    pub max_iterations: u32,
}

use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct PlaygroundArgs {
    /// Host module exporting the capabilities the script expects.
    #[arg(long, default_value = "host.harn")]
    pub host: String,
    /// Pipeline entrypoint to execute.
    #[arg(long, default_value = "pipeline.harn")]
    pub script: String,
    /// Runtime task string exposed via `runtime_task()`.
    #[arg(long)]
    pub task: Option<String>,
    /// Provider/model override as `provider:model`.
    #[arg(long)]
    pub llm: Option<String>,
    /// Replay LLM responses from a JSONL fixture file instead of
    /// calling the configured provider.
    #[arg(
        long = "llm-mock",
        value_name = "PATH",
        conflicts_with = "llm_mock_record"
    )]
    pub llm_mock: Option<String>,
    /// Record executed LLM responses into a JSONL fixture file.
    #[arg(
        long = "llm-mock-record",
        value_name = "PATH",
        conflicts_with = "llm_mock"
    )]
    pub llm_mock_record: Option<String>,
    /// Re-run when the script or host module changes.
    #[arg(long)]
    pub watch: bool,
}

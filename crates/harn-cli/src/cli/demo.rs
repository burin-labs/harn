use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct DemoArgs {
    /// Scenario to run. Omit to print the menu of available scenarios.
    pub scenario: Option<String>,
    /// List the bundled scenarios with their short descriptions.
    #[arg(long, conflicts_with = "scenario")]
    pub list: bool,
    /// Run against the configured LLM provider instead of the bundled
    /// offline tape. Surfaces a hint to re-run without `--live` when
    /// the failure looks like a missing/invalid provider key.
    #[arg(long, conflicts_with = "replay")]
    pub live: bool,
    /// Explicitly use the bundled offline tape. This is the default
    /// when neither `--live` nor `--replay` is set.
    #[arg(long)]
    pub replay: bool,
    /// Emit the demo summary as JSON instead of pretty CLI rendering.
    #[arg(long)]
    pub json: bool,
    /// Skip writing a `.harn-runs/demo-*` run record. Useful for CI
    /// smoke checks that just want to validate the tape replays.
    #[arg(long = "no-record")]
    pub no_record: bool,
}

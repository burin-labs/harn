use clap::{Args, Subcommand};

use super::util::trigger_provider_completion_parser;

#[derive(Debug, Args)]
pub(crate) struct ConnectorArgs {
    #[command(subcommand)]
    pub command: ConnectorCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConnectorCommand {
    /// Check a pure-Harn connector package against connector contract v1.
    Check(ConnectorCheckArgs),
}

#[derive(Debug, Args, Clone)]
pub(crate) struct ConnectorCheckArgs {
    /// Package directory, harn.toml, or file under the package to check.
    pub package: String,
    /// Restrict the check to one provider id. Repeatable.
    #[arg(
        long = "provider",
        value_name = "ID",
        value_parser = trigger_provider_completion_parser(),
        hide_possible_values = true
    )]
    pub providers: Vec<String>,
    /// Run poll bindings long enough to execute the first poll_tick.
    #[arg(long = "run-poll-tick")]
    pub run_poll_tick: bool,
    /// Emit the check report as JSON.
    #[arg(long)]
    pub json: bool,
}

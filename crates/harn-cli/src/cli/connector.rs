use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct ConnectorArgs {
    #[command(subcommand)]
    pub command: ConnectorCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConnectorCommand {
    /// Check a pure-Harn connector package against connector contract v1.
    Check(ConnectorCheckArgs),
    /// Run the full first-party connector package gate.
    Test(ConnectorTestArgs),
}

#[derive(Debug, Args, Clone)]
pub(crate) struct ConnectorCheckArgs {
    /// Package directory, harn.toml, or file under the package to check.
    pub package: String,
    /// Restrict the check to one provider id. Repeatable.
    #[arg(long = "provider", value_name = "ID")]
    pub providers: Vec<String>,
    /// Run poll bindings long enough to execute the first poll_tick.
    #[arg(long = "run-poll-tick")]
    pub run_poll_tick: bool,
    /// Emit the check report as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct ConnectorTestArgs {
    /// Package directory, harn.toml, or file under the package to test.
    #[arg(default_value = ".")]
    pub package: String,
    /// Restrict the gate to one provider id. Repeatable.
    #[arg(long = "provider", value_name = "ID")]
    pub providers: Vec<String>,
    /// Run poll bindings long enough to execute the first poll_tick.
    #[arg(long = "run-poll-tick")]
    pub run_poll_tick: bool,
    /// Emit the gate report as JSON.
    #[arg(long)]
    pub json: bool,
}

use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct PersonaArgs {
    #[command(subcommand)]
    pub command: PersonaCommand,
    /// Explicit harn.toml path or directory. Defaults to nearest harn.toml from cwd.
    #[arg(long, global = true, value_name = "PATH")]
    pub manifest: Option<PathBuf>,
    /// Directory used for durable persona runtime state and event-log data.
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        default_value = ".harn/personas"
    )]
    pub state_dir: PathBuf,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PersonaCommand {
    /// Validate a persona manifest with the canonical harn-modules schema.
    Check(PersonaCheckArgs),
    /// List personas declared in the resolved harn.toml.
    List(PersonaListArgs),
    /// Inspect one persona from the resolved harn.toml.
    Inspect(PersonaInspectArgs),
    /// Query durable persona lifecycle, lease, budget, and queue status.
    Status(PersonaStatusArgs),
    /// Pause a persona; matching events queue until resume drains them.
    Pause(PersonaControlArgs),
    /// Resume a persona and drain queued events once under leases.
    Resume(PersonaControlArgs),
    /// Disable a persona; matching events are recorded as dead-lettered.
    Disable(PersonaControlArgs),
    /// Fire a synthetic schedule tick for a persona.
    Tick(PersonaTickArgs),
    /// Fire a synthetic external trigger envelope for a persona.
    Trigger(PersonaTriggerArgs),
    /// Record an expensive-work budget receipt for a persona.
    Spend(PersonaSpendArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PersonaCheckArgs {
    /// Persona manifest, harn.toml path, or directory containing harn.toml.
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// Emit typed validation errors as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaListArgs {
    /// Emit a stable JSON array instead of a human-readable table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaInspectArgs {
    /// Persona name to inspect.
    pub name: String,
    /// Emit stable JSON for Harn Cloud, Burin Code, or other hosts.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaStatusArgs {
    /// Persona name to query.
    pub name: String,
    /// RFC3339 timestamp to use for deterministic budget windows. When
    /// omitted, falls back to the current UTC wall clock.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Emit stable JSON for Harn Cloud, Burin Code, or other hosts.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaControlArgs {
    /// Persona name to control.
    pub name: String,
    /// RFC3339 timestamp to use as "now" for deterministic tests.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Emit stable JSON after applying the control.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaTickArgs {
    /// Persona name to wake from its schedule binding.
    pub name: String,
    /// RFC3339 timestamp to use for deterministic tests.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Estimated expensive-work cost for budget enforcement.
    #[arg(long, default_value_t = 0.0)]
    pub cost_usd: f64,
    /// Estimated token count for budget enforcement.
    #[arg(long, default_value_t = 0)]
    pub tokens: u64,
    /// Emit stable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaTriggerArgs {
    /// Persona name to wake from an external trigger.
    pub name: String,
    /// Provider name, for example github, linear, slack, or webhook.
    #[arg(long)]
    pub provider: String,
    /// Provider event kind, for example pull_request, check_run, issue, or message.
    #[arg(long)]
    pub kind: String,
    /// Normalized metadata as key=value. Repeat for multiple fields.
    #[arg(long = "metadata", value_name = "KEY=VALUE")]
    pub metadata: Vec<String>,
    /// RFC3339 timestamp to use for deterministic tests.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Estimated expensive-work cost for budget enforcement.
    #[arg(long, default_value_t = 0.0)]
    pub cost_usd: f64,
    /// Estimated token count for budget enforcement.
    #[arg(long, default_value_t = 0)]
    pub tokens: u64,
    /// Emit stable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaSpendArgs {
    /// Persona name to charge.
    pub name: String,
    /// Cost in USD to record.
    #[arg(long)]
    pub cost_usd: f64,
    /// Tokens to record.
    #[arg(long, default_value_t = 0)]
    pub tokens: u64,
    /// RFC3339 timestamp to use for deterministic tests.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Emit stable JSON.
    #[arg(long)]
    pub json: bool,
}

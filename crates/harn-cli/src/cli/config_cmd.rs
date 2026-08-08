use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConfigCommand {
    /// Print the merged runtime config.
    Inspect(ConfigInspectArgs),
    /// Validate local, project, or managed config overlays.
    Validate(ConfigValidateArgs),
    /// Print the editor JSON Schema for Harn config files.
    Schema(ConfigSchemaArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ConfigInspectArgs {
    /// Include per-field provenance, shadowed candidates, and lock status.
    #[arg(long)]
    pub explain: bool,
    /// Do not discover OS/user/project config files; only built-ins, explicit inputs, and env.
    #[arg(long)]
    pub no_discovery: bool,
    /// Additional local/project config overlay to merge at project precedence.
    #[arg(long = "config", value_name = "PATH")]
    pub config_files: Vec<PathBuf>,
    /// Managed policy overlay to merge before environment overrides.
    #[arg(long = "managed", value_name = "PATH")]
    pub managed_files: Vec<PathBuf>,
    /// Remote default config URL. Requires HARN_CONFIG_TRUST_REMOTE=1.
    #[arg(long = "remote-defaults-url", value_name = "URL")]
    pub remote_defaults_url: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ConfigValidateArgs {
    /// Config files to validate. Defaults to discovered files.
    #[arg(value_name = "PATH")]
    pub files: Vec<PathBuf>,
    /// Treat input files as managed policy overlays when reporting them.
    #[arg(long)]
    pub managed: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ConfigSchemaArgs {
    /// Write the schema JSON to this path instead of stdout.
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,
}

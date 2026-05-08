use std::path::PathBuf;

use clap::{ArgAction, Args};

#[derive(Debug, Args)]
pub(crate) struct PortalArgs {
    /// Directory containing persisted run records.
    #[arg(long, default_value = ".harn-runs")]
    pub dir: String,
    /// Explicit harn.toml path or directory for persona catalog APIs.
    #[arg(long, value_name = "PATH")]
    pub manifest: Option<PathBuf>,
    /// Directory used for durable persona runtime state.
    #[arg(long, value_name = "DIR", default_value = ".harn/personas")]
    pub persona_state_dir: PathBuf,
    /// Host interface to bind.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    /// Port to serve the portal on.
    #[arg(long, default_value_t = 4721)]
    pub port: u16,
    /// Open the portal in a browser after starting.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub open: bool,
}

use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct DoctorArgs {
    /// Skip provider connectivity checks.
    #[arg(long)]
    pub no_network: bool,
    /// Emit a single JSON document instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

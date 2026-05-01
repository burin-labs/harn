use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct DoctorArgs {
    /// Skip provider connectivity checks.
    #[arg(long)]
    pub no_network: bool,
}

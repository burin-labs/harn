use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct VerifyArgs {
    /// Path to the signed provenance receipt JSON file.
    pub receipt: String,
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

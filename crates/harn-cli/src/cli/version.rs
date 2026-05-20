use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct VersionArgs {
    /// Emit a structured `JsonEnvelope` carrying the build metadata
    /// instead of the human-readable banner. The envelope payload is
    /// `{ name, version, description }`.
    ///
    /// See `docs/src/cli-json-contract.md` for the envelope shape.
    #[arg(long)]
    pub json: bool,
}

use std::path::PathBuf;

use clap::Args;

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct PackArgs {
    /// Entrypoint `.harn` file to pack. Transitive imports under the
    /// entrypoint's directory are bundled alongside it.
    pub entrypoint: PathBuf,

    /// Output `.harnpack` path. Defaults to the entrypoint stem with
    /// the `.harnpack` extension next to the entrypoint.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,

    /// Read an existing `.harnpack` and re-emit it under the v2
    /// manifest, preserving the prior bundle's id, name, version,
    /// triggers, workflow graph, and prompt capsules. The new
    /// `<entrypoint>` argument supplies the transitive-modules /
    /// SBOM payload that v1 lacked.
    #[arg(long, value_name = "OLD_BUNDLE")]
    pub upgrade: Option<PathBuf>,

    /// Mark the bundle as unsigned. Signing lands in E6.3; today this
    /// flag is documentary so scripts can opt into the future
    /// `--sign`/`--unsigned` split without breaking.
    #[arg(long, default_value_t = false)]
    pub unsigned: bool,

    /// Emit a `JsonEnvelope` summary instead of a human-readable
    /// one-liner. Schema: `harn --json-schemas --command pack`.
    #[arg(long, default_value_t = false)]
    pub json: bool,
}

use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct PackArgs {
    #[command(subcommand)]
    pub command: Option<PackCommand>,

    /// Entrypoint `.harn` file to pack. Transitive imports under the
    /// entrypoint's directory are bundled alongside it.
    ///
    /// When `harn pack` is invoked without a subcommand, this positional
    /// argument selects the build path; passing a subcommand (e.g.
    /// `harn pack verify <bundle>`) routes to that subcommand instead.
    #[arg(required = false)]
    pub entrypoint: Option<PathBuf>,

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

    /// Sign the bundle hash and embed an Ed25519 signature in the manifest.
    #[arg(
        long,
        default_value_t = false,
        conflicts_with = "unsigned",
        requires = "key"
    )]
    pub sign: bool,

    /// Ed25519 private key PEM used with `--sign`.
    #[arg(long, value_name = "PATH", requires = "sign")]
    pub key: Option<PathBuf>,

    /// Mark the bundle as unsigned. This still emits an OpenTrustGraph
    /// release record at autonomy tier `suggest`.
    #[arg(long, default_value_t = false)]
    pub unsigned: bool,

    /// Refuse to bundle modules whose path matches a built-in
    /// secret-bearing glob (`.env`, `.env.*`, `*.pem`, `*.key`,
    /// `credentials*`, anything under `secrets/`). The default behavior
    /// matches the historical pack semantics: pack the full transitive
    /// module set without any secret filtering. Pass `--exclude-secrets`
    /// from CI or release pipelines that share bundles externally.
    ///
    /// Today the static module walk is rooted at the entrypoint
    /// directory and only pulls in `.harn` files; the gate primarily
    /// blocks an entrypoint that itself looks like a secret-bearing
    /// path and is wired so the CLI / JSON surface stays stable when
    /// asset bundling lands.
    #[arg(long, default_value_t = false, conflicts_with = "include_secrets")]
    pub exclude_secrets: bool,

    /// Explicitly opt in to the default behavior: bundle every
    /// transitive module without secret filtering. Useful in scripts
    /// that want to be explicit about the bundle's contents instead of
    /// relying on the default.
    #[arg(long, default_value_t = false)]
    pub include_secrets: bool,

    /// Emit a `JsonEnvelope` summary instead of a human-readable
    /// one-liner. Schema: `harn --json-schemas --command pack`.
    #[arg(long, default_value_t = false)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum PackCommand {
    /// Verify a `.harnpack` bundle: check the embedded Ed25519
    /// signature (if present), recompute the canonical bundle hash,
    /// and compare each archive entry's BLAKE3 against the manifest's
    /// recorded hashes. Exits non-zero on any mismatch.
    Verify(PackVerifyArgs),
}

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct PackVerifyArgs {
    /// Path to the `.harnpack` archive to verify.
    pub bundle: PathBuf,

    /// Accept bundles that carry no Ed25519 signature. Without this
    /// flag, an unsigned bundle is treated as a verification failure.
    #[arg(long, default_value_t = false)]
    pub allow_unsigned: bool,

    /// Emit a `JsonEnvelope` summary instead of a human-readable
    /// one-liner. Schema: `harn --json-schemas --command "pack verify"`.
    #[arg(long, default_value_t = false)]
    pub json: bool,
}

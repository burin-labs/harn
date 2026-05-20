use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct PackArgs {
    #[command(subcommand)]
    pub command: Option<PackCommand>,

    /// Entrypoint `.harn` file to pack. Transitive Harn modules and
    /// non-Harn assets referenced by import directives under the
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
    /// The same gate skips imported non-Harn assets that match the
    /// secret heuristic and reports each skipped asset as a structured
    /// JSON warning plus `manifest.metadata.skipped_assets`.
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

    /// JSON trust policy describing the signer registry URL and
    /// optional trusted signer allowlist to enforce during
    /// verification.
    #[arg(long, value_name = "PATH")]
    pub trust_policy: Option<PathBuf>,

    /// Require the bundle signer to resolve from the trusted signer
    /// registry and, when `--trust-policy` supplies a
    /// `trusted_signers` allowlist, appear in that allowlist too.
    #[arg(long, default_value_t = false)]
    pub require_trusted_signer: bool,

    /// Cross-check SBOM package hashes against the archive payloads
    /// they describe when the bundle format carries a corresponding
    /// entry.
    #[arg(long, default_value_t = false)]
    pub strict: bool,

    /// Emit a `JsonEnvelope` summary instead of a human-readable
    /// one-liner. Schema: `harn --json-schemas --command "pack verify"`.
    #[arg(long, default_value_t = false)]
    pub json: bool,
}

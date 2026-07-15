use std::path::PathBuf;

use clap::{Args, Subcommand};

use super::provider::ProviderCatalogShowArgs;

#[derive(Debug, Args)]
pub(crate) struct ProviderCatalogArgs {
    #[command(subcommand)]
    pub command: ProviderCatalogCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ProviderCatalogCommand {
    /// Run the provider catalog refresh workflow.
    Refresh(ProvidersRefreshArgs),
    /// Validate the loaded catalog and generated artifact contract.
    Validate(ProvidersValidateArgs),
    /// Generate or check every provider catalog artifact from source fragments.
    Generate(ProvidersGenerateArgs),
    /// Export provider catalog artifacts for an explicit overlay.
    Export(ProvidersExportArgs),
    /// Generate or check the provider capability matrix docs.
    Matrix(ProvidersMatrixArgs),
    /// Generate or check provider recommendation docs and JSON support data.
    Support(ProvidersSupportArgs),
    /// Recommend local provider/model presets from coding-agent readiness data.
    Recommend(ProvidersRecommendArgs),
    /// Print the provider/model catalog Harn loaded as JSON.
    Show(ProviderCatalogShowArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ProvidersRefreshArgs {
    /// Hit live provider/model sources instead of bundled fixtures.
    #[arg(long)]
    pub live: bool,
    /// Compare fixture output with committed refresh goldens.
    #[arg(long)]
    pub check: bool,
    /// Refresh committed refresh goldens. Implies --check.
    #[arg(long)]
    pub update: bool,
    /// Refresh workflow script path.
    #[arg(long, default_value = "scripts/update_provider_catalog.harn")]
    pub script: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct ProvidersValidateArgs {
    /// Merge an additional providers.toml-style overlay before validating.
    #[arg(long)]
    pub overlay: Option<PathBuf>,
    /// Merge a capabilities.toml-style overlay (same layout as the built-in
    /// capability matrix) before validating, so overlay-declared private or
    /// local models can claim structured capabilities.
    #[arg(long = "capabilities-overlay")]
    pub capabilities_overlay: Option<PathBuf>,
    /// Emit validation details as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ProvidersGenerateArgs {
    /// Directory containing provider catalog TOML source fragments.
    #[arg(long, default_value = "crates/harn-vm/src/llm/catalog_sources")]
    pub source_dir: PathBuf,
    /// Directory containing provider capability TOML source fragments.
    #[arg(long, default_value = "crates/harn-vm/src/llm/capability_sources")]
    pub capability_source_dir: PathBuf,
    /// Generated embedded provider catalog snapshot.
    #[arg(long, default_value = "crates/harn-vm/src/llm/providers.toml")]
    pub providers_output: PathBuf,
    /// Generated embedded provider capability snapshot.
    #[arg(long, default_value = "crates/harn-vm/src/llm/capabilities.toml")]
    pub capabilities_output: PathBuf,
    /// Directory for generated provider catalog JSON, schema, and type artifacts.
    #[arg(long, default_value = "spec/provider-catalog")]
    pub artifact_dir: PathBuf,
    /// Check whether every generated artifact is up to date without writing.
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ProvidersExportArgs {
    /// Directory to write generated artifacts into.
    #[arg(long, default_value = "spec/provider-catalog")]
    pub output_dir: PathBuf,
    /// Check whether existing files are up to date without writing them.
    #[arg(long)]
    pub check: bool,
    /// Merge an additional providers.toml-style overlay before exporting.
    #[arg(long)]
    pub overlay: Option<PathBuf>,
    /// Merge a capabilities.toml-style overlay (same layout as the built-in
    /// capability matrix) before exporting, so overlay-declared private or
    /// local models can claim structured capabilities. The serve runtime
    /// honors the same file through the manifest `[capabilities]` section,
    /// keeping exported and served catalogs in agreement.
    #[arg(long = "capabilities-overlay")]
    pub capabilities_overlay: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct ProvidersMatrixArgs {
    /// Path for the generated markdown matrix.
    #[arg(long, default_value = "docs/src/provider-matrix.md")]
    pub output: PathBuf,
    /// Check whether the matrix file is up to date without writing it.
    #[arg(long)]
    pub check: bool,
    /// Print the generated markdown to stdout instead of writing it.
    #[arg(long)]
    pub stdout: bool,
    /// Only include matrix rows that support the named feature.
    #[arg(long)]
    pub filter: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ProvidersSupportArgs {
    /// Path for the generated markdown recommendations page.
    #[arg(long, default_value = "docs/src/provider-support.md")]
    pub output: PathBuf,
    /// Path for the generated JSON support data.
    #[arg(long = "json-output", default_value = "docs/provider-support.json")]
    pub json_output: PathBuf,
    /// Hand-written provider support note overrides.
    #[arg(
        long,
        default_value = "crates/harn-cli/data/provider_support_notes.toml"
    )]
    pub notes: PathBuf,
    /// Coding-agent summary JSON to merge into empirical support rows.
    #[arg(long = "empirical")]
    pub empirical: Vec<PathBuf>,
    /// Check whether markdown and JSON artifacts are up to date without writing.
    #[arg(long)]
    pub check: bool,
    /// Print the generated markdown to stdout instead of writing artifacts.
    #[arg(long, conflicts_with = "json")]
    pub stdout: bool,
    /// Print generated JSON to stdout instead of writing artifacts.
    #[arg(long, conflicts_with = "stdout")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ProvidersRecommendArgs {
    /// Local readiness JSON or coding-agent summary JSON to read.
    #[arg(long)]
    pub input: Option<PathBuf>,
    /// Coding-agent summary JSON to convert into a readiness report.
    #[arg(long, conflicts_with = "input")]
    pub summary: Option<PathBuf>,
    /// Limit recommendations and outcomes to one provider id.
    #[arg(long)]
    pub provider: Option<String>,
    /// Emit the structured local readiness report as JSON.
    #[arg(long)]
    pub json: bool,
}

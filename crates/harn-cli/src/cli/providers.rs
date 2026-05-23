use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct ProvidersArgs {
    #[command(subcommand)]
    pub command: ProvidersCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ProvidersCommand {
    /// Run the provider catalog refresh workflow.
    Refresh(ProvidersRefreshArgs),
    /// Validate the loaded catalog and generated artifact contract.
    Validate(ProvidersValidateArgs),
    /// Generate JSON, schema, TypeScript, and Swift catalog artifacts.
    Export(ProvidersExportArgs),
    /// Generate or check the provider capability matrix docs.
    Matrix(ProvidersMatrixArgs),
    /// Recommend local provider/model presets from coding-agent readiness data.
    Recommend(ProvidersRecommendArgs),
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
    /// Also verify checked-in generated artifacts match the current catalog.
    #[arg(long = "check-artifacts")]
    pub check_artifacts: bool,
    /// Directory containing generated provider catalog artifacts.
    #[arg(long, default_value = "spec/provider-catalog")]
    pub artifact_dir: PathBuf,
    /// Emit validation details as JSON.
    #[arg(long)]
    pub json: bool,
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

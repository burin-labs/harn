use std::path::PathBuf;

use clap::{ArgAction, Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct InstallArgs {
    /// Fail if harn.lock would need to change.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub frozen: bool,
    /// Alias for --frozen, intended for CI and production installs.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub locked: bool,
    /// Do not fetch from package sources; use only harn.lock and the local cache.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue, conflicts_with = "refetch")]
    pub offline: bool,
    /// Force refetching one dependency (or every dependency with `--refetch all`).
    #[arg(long, value_name = "ALIAS|all")]
    pub refetch: Option<String>,
    /// Emit a JSON install summary instead of a human-readable line.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AddArgs {
    /// Git URL/ref spec, local package path, or dependency name in the legacy `--git/--path` form.
    pub name_or_spec: String,
    /// Override the dependency alias written under `[dependencies]`.
    #[arg(long)]
    pub alias: Option<String>,
    /// Git URL for a remote dependency.
    #[arg(long, conflicts_with = "path")]
    pub git: Option<String>,
    /// Deprecated alias for `--rev` on the legacy `--git` form.
    #[arg(long, conflicts_with_all = ["rev", "branch"])]
    pub tag: Option<String>,
    /// Git rev to pin for a remote dependency.
    #[arg(long, conflicts_with = "branch")]
    pub rev: Option<String>,
    /// Git branch to track in the manifest; the lockfile still pins a commit.
    #[arg(long, conflicts_with_all = ["rev", "tag"])]
    pub branch: Option<String>,
    /// Local path for a path dependency.
    #[arg(long, conflicts_with = "git")]
    pub path: Option<String>,
    /// Package registry index URL or path for registry-name dependencies.
    #[arg(long, value_name = "URL|PATH")]
    pub registry: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct UpdateArgs {
    /// Refresh every dependency instead of one alias.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub all: bool,
    /// Refresh only this dependency alias.
    pub alias: Option<String>,
    /// Emit a JSON update summary instead of a human-readable line.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct RemoveArgs {
    /// Dependency alias to remove from harn.toml and harn.lock.
    pub alias: String,
}

#[derive(Debug, Args)]
pub(crate) struct PackageArgs {
    #[command(subcommand)]
    pub command: PackageCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PackageCommand {
    /// List locked and materialized packages plus exported surfaces.
    List(PackageListArgs),
    /// Diagnose harn.toml, harn.lock, installed packages, and host requirements.
    Doctor(PackageDoctorArgs),
    /// Search the package registry index.
    Search(PackageSearchArgs),
    /// Show package registry metadata for one package.
    Info(PackageInfoArgs),
    /// Validate a package manifest and publish readiness.
    Check(PackageCheckArgs),
    /// Build an inspectable package artifact directory.
    Pack(PackagePackArgs),
    /// Generate package API docs from exported Harn symbols.
    Docs(PackageDocsArgs),
    /// Inspect, clean, and verify the shared package cache.
    Cache(PackageCacheArgs),
    /// Report dependencies whose lockfile entries have newer registry or branch HEAD versions.
    Outdated(PackageOutdatedArgs),
    /// Audit harn.lock provenance, package compatibility, and supply-chain integrity.
    Audit(PackageAuditArgs),
    /// Inspect or verify the published Harn protocol-artifact contract.
    Artifacts(PackageArtifactsArgs),
    /// Scaffold a package from an external source.
    Scaffold(Box<PackageScaffoldArgs>),
}

#[derive(Debug, Args)]
pub(crate) struct PackageScaffoldArgs {
    #[command(subcommand)]
    pub command: PackageScaffoldCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PackageScaffoldCommand {
    /// Generate a focused Harn SDK package from an OpenAPI 3.1 spec.
    Openapi(PackageScaffoldOpenapiArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PackageScaffoldOpenapiArgs {
    /// Harn package name, for example `acme-sdk-harn`.
    #[arg(long)]
    pub name: String,
    /// Generated Harn module/export name. Defaults to a snake_case form of --name.
    #[arg(long = "module-name")]
    pub module_name: Option<String>,
    /// Generated client constructor type label. Defaults to `<ModuleName>Client`.
    #[arg(long = "client-name")]
    pub client_name: Option<String>,
    /// Local OpenAPI JSON/YAML file or HTTPS URL.
    #[arg(long, value_name = "PATH_OR_URL")]
    pub spec: String,
    /// Destination package directory. Defaults to `<name>/`.
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,
    /// One-line package description.
    #[arg(long)]
    pub description: Option<String>,
    /// Default base URL baked into `new_client()`. Defaults to the first OpenAPI server URL.
    #[arg(long = "default-base-url")]
    pub default_base_url: Option<String>,
    /// Local `harn-openapi` checkout to use for deterministic generation.
    #[arg(long = "harn-openapi-path", value_name = "DIR")]
    pub harn_openapi_path: Option<PathBuf>,
    /// Git URL or local git repo path written for the generated `harn-openapi` dependency.
    #[arg(long = "harn-openapi-git", value_name = "URL|PATH")]
    pub harn_openapi_git: Option<String>,
    /// Git rev/tag written for the generated `harn-openapi` dependency.
    #[arg(long = "harn-openapi-rev", conflicts_with = "harn_openapi_branch")]
    pub harn_openapi_rev: Option<String>,
    /// Git branch written for the generated `harn-openapi` dependency. Defaults to `main`.
    #[arg(long = "harn-openapi-branch", conflicts_with = "harn_openapi_rev")]
    pub harn_openapi_branch: Option<String>,
    /// Overwrite generated files in an existing package directory.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackageListArgs {
    /// Emit JSON instead of a human-readable report.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackageDoctorArgs {
    /// Emit JSON instead of a human-readable report.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackageCheckArgs {
    /// Package directory, harn.toml, or file under the package. Defaults to cwd.
    pub package: Option<PathBuf>,
    /// Emit JSON instead of a human-readable report.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackagePackArgs {
    /// Package directory, harn.toml, or file under the package. Defaults to cwd.
    pub package: Option<PathBuf>,
    /// Output artifact directory. Defaults to .harn/dist/<name>-<version>.
    #[arg(long, value_name = "DIR")]
    pub output: Option<PathBuf>,
    /// Validate and print the file list without writing the artifact.
    #[arg(long)]
    pub dry_run: bool,
    /// Emit JSON instead of a human-readable report.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackageDocsArgs {
    /// Package directory, harn.toml, or file under the package. Defaults to cwd.
    pub package: Option<PathBuf>,
    /// Output Markdown file. Defaults to docs/api.md.
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,
    /// Verify the output file already matches generated docs.
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackageSearchArgs {
    /// Search query. Omit to list all registry packages.
    pub query: Option<String>,
    /// Package registry index URL or path.
    #[arg(long, value_name = "URL|PATH")]
    pub registry: Option<String>,
    /// Emit JSON instead of a tab-separated table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackageInfoArgs {
    /// Registry package name, optionally with @version.
    pub name: String,
    /// Package registry index URL or path.
    #[arg(long, value_name = "URL|PATH")]
    pub registry: Option<String>,
    /// Emit JSON instead of human-readable metadata.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PublishArgs {
    /// Package directory, harn.toml, or file under the package. Defaults to cwd.
    pub package: Option<PathBuf>,
    /// Required until registry submission support lands.
    #[arg(long)]
    pub dry_run: bool,
    /// Package registry index URL or path to report as the submission target.
    #[arg(long, value_name = "URL|PATH")]
    pub registry: Option<String>,
    /// Emit JSON instead of a human-readable report.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackageCacheArgs {
    #[command(subcommand)]
    pub command: PackageCacheCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PackageCacheCommand {
    /// List cached git package entries.
    List,
    /// Remove cached git package entries not referenced by harn.lock, or every entry with --all.
    Clean(PackageCacheCleanArgs),
    /// Verify cached package contents against harn.lock.
    Verify(PackageCacheVerifyArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PackageCacheCleanArgs {
    /// Remove every package cache entry instead of only entries unused by the current harn.lock.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackageCacheVerifyArgs {
    /// Also verify materialized packages under .harn/packages/.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub materialized: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackageOutdatedArgs {
    /// Resolve registry latest versions over the network instead of relying on the local index cache.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub refresh: bool,
    /// Probe git remotes for branch-tracking dependencies. Requires git in PATH.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub remote: bool,
    /// Override the registry index URL or path.
    #[arg(long, value_name = "URL|PATH")]
    pub registry: Option<String>,
    /// Emit JSON instead of a human-readable report.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackageAuditArgs {
    /// Override the registry index URL or path used for yanked-version checks.
    #[arg(long, value_name = "URL|PATH")]
    pub registry: Option<String>,
    /// Skip integrity hashing of materialized packages under .harn/packages/.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub skip_materialized: bool,
    /// Emit JSON instead of a human-readable report.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PackageArtifactsArgs {
    #[command(subcommand)]
    pub command: PackageArtifactsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PackageArtifactsCommand {
    /// Print the protocol-artifact manifest the running Harn would generate.
    Manifest(PackageArtifactsManifestArgs),
    /// Compare a downstream-vendored manifest against the running Harn manifest and report drift.
    Check(PackageArtifactsCheckArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PackageArtifactsManifestArgs {
    /// Write the manifest to this path instead of stdout.
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct PackageArtifactsCheckArgs {
    /// Path to the vendored protocol-artifact manifest the downstream consumer is shipping.
    pub manifest: PathBuf,
    /// Emit JSON instead of a human-readable report.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub json: bool,
}

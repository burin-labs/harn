use clap::{ArgGroup, Args, ValueEnum};

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("matrix")
        .args(["provider_matrix", "connector_matrix"])
        .multiple(false)
))]
pub(crate) struct CheckArgs {
    /// Print the provider/model capability matrix instead of checking files.
    #[arg(long = "provider-matrix")]
    pub provider_matrix: bool,
    /// Print the connector package capability matrix instead of checking files.
    #[arg(long = "connector-matrix")]
    pub connector_matrix: bool,
    /// Emit a structured `JsonEnvelope` report. For matrix commands, replaces
    /// the deprecated `--format=json` spelling.
    #[arg(long)]
    pub json: bool,
    /// Output format for matrix commands.
    #[arg(long = "format", value_enum, default_value_t = CheckOutputFormat::Text, requires = "matrix")]
    pub format: CheckOutputFormat,
    /// Only include matrix rows that support the named feature.
    #[arg(long = "filter", value_name = "FEATURE", requires = "matrix")]
    pub filter: Option<String>,
    /// Extra host capability schema for preflight validation.
    #[arg(long = "host-capabilities")]
    pub host_capabilities: Option<String>,
    /// Alternate root for render/template path checks.
    #[arg(long = "bundle-root")]
    pub bundle_root: Option<String>,
    /// Treat warnings as failures. Monotonically enables `[check] strict` for
    /// this invocation; it never disables workspace strictness.
    #[arg(long)]
    pub strict: bool,
    /// Flag unvalidated boundary-API values used in field access.
    #[arg(long = "strict-types")]
    pub strict_types: bool,
    /// Check every `.harn` file under `[workspace].pipelines` in the
    /// nearest `harn.toml`. Positional targets are additive.
    #[arg(long = "workspace")]
    pub workspace: bool,
    /// Downgrade preflight diagnostics to warnings (or suppress them
    /// entirely with `off`). Overrides `[check].preflight_severity`.
    /// Accepted values: `error` (default), `warning`, `off`.
    #[arg(long = "preflight", value_name = "SEVERITY")]
    pub preflight: Option<String>,
    /// Evaluate `@invariant(...)` annotations and fail on violations.
    #[arg(long = "invariants")]
    pub invariants: bool,
    /// Analyze each collected source as an independent target while sharing
    /// this process and its bounded worker pool. This preserves one-file
    /// module resolution for fixture corpora without paying one CLI startup
    /// per file.
    #[arg(long = "independent")]
    pub independent: bool,
    /// One or more .harn files or directories. Optional when `--workspace`
    /// is set.
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum CheckOutputFormat {
    Text,
    Json,
    Markdown,
}

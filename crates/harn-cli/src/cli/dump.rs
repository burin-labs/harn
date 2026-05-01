use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct DumpHighlightKeywordsArgs {
    /// Path to the generated keyword file (relative to the repo root).
    #[arg(long, default_value = "docs/theme/harn-keywords.js")]
    pub output: String,
    /// Verify the on-disk file matches what would be generated; exit non-zero
    /// if stale. Used by CI to prevent drift between the highlighter and the
    /// lexer/stdlib.
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DumpTriggerQuickrefArgs {
    /// Path to the generated trigger quickref file (relative to the repo root).
    #[arg(long, default_value = "docs/llm/harn-triggers-quickref.md")]
    pub output: String,
    /// Verify the on-disk file matches what would be generated; exit non-zero
    /// if stale. Used by CI to prevent drift between the quickref and the
    /// runtime trigger provider catalog.
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DumpProviderMatrixArgs {
    /// Path to the generated provider matrix page (relative to the repo root).
    #[arg(long, default_value = "docs/src/provider-matrix.md")]
    pub output: String,
    /// Verify the on-disk file matches what would be generated; exit non-zero
    /// if stale. Used by CI to prevent drift from capabilities.toml.
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DumpConnectorMatrixArgs {
    /// Path to the generated connector parity matrix page (relative to the repo root).
    #[arg(long, default_value = "docs/src/connectors/parity-matrix.md")]
    pub output: String,
    /// Package manifest, package directory, or directory tree to scan. Repeatable.
    #[arg(long = "source", value_name = "PATH")]
    pub sources: Vec<String>,
    /// Verify the on-disk file matches what would be generated; exit non-zero
    /// if stale. Used by CI to prevent drift from connector package manifests.
    #[arg(long)]
    pub check: bool,
}

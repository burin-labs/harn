use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct PathTargetsArgs {
    /// Automatically apply safe fixes.
    #[arg(long)]
    pub fix: bool,
    /// Force-enable the `require-file-header` rule (overrides harn.toml).
    #[arg(long = "require-file-header")]
    pub require_file_header: bool,
    /// One or more .harn files or directories.
    #[arg(required = true)]
    pub targets: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct FmtArgs {
    /// Check formatting without rewriting files.
    #[arg(long)]
    pub check: bool,
    /// Emit a structured `JsonEnvelope` report instead of human-readable output.
    #[arg(long)]
    pub json: bool,
    /// Maximum line width before wrapping. Overrides `[fmt] line_width` in harn.toml.
    #[arg(long = "line-width")]
    pub line_width: Option<usize>,
    /// Total width of `// ----` separator bars. Defaults to line width minus indent.
    /// Overrides `[fmt] separator_width`.
    #[arg(long = "separator-width")]
    pub separator_width: Option<usize>,
    /// One or more .harn files or directories.
    #[arg(required = true)]
    pub targets: Vec<String>,
}

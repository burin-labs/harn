use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct ChatArgs {
    /// Model to talk to (id or catalog alias). Defaults to the active local
    /// selection from `harn local switch`, then the configured provider.
    pub model: Option<String>,
    /// Provider to route through. Defaults to whatever the catalog resolves
    /// for the model.
    #[arg(long)]
    pub provider: Option<String>,
    /// System prompt for the session. Omitted by default so the model answers
    /// as it would with no instructions.
    #[arg(long)]
    pub system: Option<String>,
    /// Report the full per-response timing breakdown instead of one line.
    #[arg(long)]
    pub verbose: bool,
    /// Do not report per-response speed stats.
    #[arg(long = "no-stats", conflicts_with = "verbose")]
    pub no_stats: bool,
}

impl ChatArgs {
    /// The stats mode the REPL script should start in.
    pub(crate) fn stats_mode(&self) -> &'static str {
        if self.no_stats {
            "off"
        } else if self.verbose {
            "verbose"
        } else {
            "compact"
        }
    }
}

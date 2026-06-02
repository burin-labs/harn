use clap::{Args, Subcommand};

/// `harn rule` — author and test structural rules.
#[derive(Debug, Args)]
pub(crate) struct RuleArgs {
    #[command(subcommand)]
    pub command: RuleCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RuleCommand {
    /// Run a rule's inline-annotation tests (`// ruleid:` / `// ok:`).
    Test(RuleTestArgs),
}

/// `harn rule test` — check each rule against its annotated fixture.
#[derive(Debug, Args)]
pub(crate) struct RuleTestArgs {
    /// A rule TOML file, or a directory of rules + fixtures (default: `.`).
    #[arg(value_name = "PATH")]
    pub path: Option<String>,
    /// Emit a JSON envelope instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

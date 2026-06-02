use clap::Args;

/// `harn codemod` — apply a codemod rule's `fix` across a fileset.
///
/// **Dry-run by default** (prints a unified diff per file); pass `--apply` to
/// write. A rule whose `safety` is above the machine-applicable tier needs
/// `--allow-unsafe` to apply.
#[derive(Debug, Args)]
pub(crate) struct CodemodArgs {
    /// Files or directories to rewrite (default: the current directory).
    #[arg(value_name = "PATHS")]
    pub paths: Vec<String>,
    /// Run a codemod rule from a TOML file (must declare a `fix`).
    #[arg(long = "rule", value_name = "FILE")]
    pub rule: Option<String>,
    /// Run every `*.toml` rule in a directory, installed package, or built-in pack.
    #[arg(long = "rule-pack", value_name = "PACK", conflicts_with = "rule")]
    pub rule_pack: Option<String>,
    /// Write the fixes to disk. Without this, codemod is a dry-run preview.
    #[arg(long)]
    pub apply: bool,
    /// Apply even fixes above the machine-applicable safety tier.
    #[arg(long = "allow-unsafe")]
    pub allow_unsafe: bool,
    /// Emit a JSON envelope instead of human-readable diffs.
    #[arg(long)]
    pub json: bool,
}

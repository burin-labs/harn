use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct ToolArgs {
    #[command(subcommand)]
    pub command: ToolCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ToolCommand {
    /// Scaffold a Harn package that exports one custom tool.
    New(ToolNewArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ToolNewArgs {
    /// Tool/package name. May use dashes; generated Harn identifiers use underscores.
    pub name: String,
    /// One-line tool description.
    #[arg(long)]
    pub description: Option<String>,
    /// Destination directory. Defaults to `<name>/`.
    #[arg(long = "dir", value_name = "PATH")]
    pub dir: Option<String>,
    /// Overwrite an existing generated directory.
    #[arg(long)]
    pub force: bool,
}

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
    /// Run a script-published tool registry as a generated command tree.
    #[command(disable_help_flag = true)]
    Run(ToolRunArgs),
    /// Print the canonical harn-tools catalog for a script-published registry.
    Schema(ToolSchemaArgs),
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

#[derive(Debug, Args)]
pub(crate) struct ToolRunArgs {
    /// Harn script whose main pipeline publishes a registry with mcp_tools(...).
    pub file: String,
    /// Generated command path and flags. `--help` renders registry-derived help.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub arguments: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ToolSchemaArgs {
    /// Harn script whose main pipeline publishes a registry with mcp_tools(...).
    pub file: String,
    /// Pretty-print the JSON catalog.
    #[arg(long)]
    pub pretty: bool,
}

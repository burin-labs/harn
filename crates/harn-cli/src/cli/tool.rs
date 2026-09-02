use clap::{Args, Subcommand, ValueEnum};

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
    /// Generate shell completion for a script-published command tree.
    Completions(ToolCompletionsArgs),
    /// Print the canonical harn-tools catalog for a script-published registry.
    Schema(ToolSchemaArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ToolCompletionsArgs {
    /// Harn script whose main pipeline publishes a tool registry.
    pub file: String,
    /// Shell whose static completion program should be emitted.
    #[arg(long, value_enum)]
    pub shell: ToolCompletionShell,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ToolCompletionShell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
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
    /// Harn script to inspect.
    pub file: String,
    /// Tool owner to inspect: execute a published registry or statically read exports.
    #[arg(long, value_enum, default_value_t = ToolSchemaSurface::Script)]
    pub surface: ToolSchemaSurface,
    /// Pretty-print the JSON catalog.
    #[arg(long)]
    pub pretty: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum ToolSchemaSurface {
    /// Execute main and inspect its script-published tool registry.
    #[default]
    Script,
    /// Compile public exports and their types without executing the script.
    Exports,
}

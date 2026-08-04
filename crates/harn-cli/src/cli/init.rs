use clap::{Args, ValueEnum};

#[derive(Debug, Args)]
pub(crate) struct InitArgs {
    /// Optional project name to scaffold.
    pub name: Option<String>,
    /// Starter template to scaffold.
    #[arg(long, value_enum, default_value_t = ProjectTemplate::Basic)]
    pub template: ProjectTemplate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ProjectTemplate {
    Basic,
    Agent,
    Chat,
    #[value(name = "mcp-server")]
    McpServer,
    Eval,
    #[value(name = "pipeline-lab")]
    PipelineLab,
    Package,
    Connector,
}

#[derive(Debug, Args)]
pub(crate) struct NewArgs {
    /// Project name, or `package` / `connector` when using `harn new package NAME`.
    pub first: Option<String>,
    /// Package or connector name when the first positional argument is a kind.
    pub second: Option<String>,
    /// Starter template to scaffold.
    #[arg(long, value_enum)]
    pub template: Option<ProjectTemplate>,
}

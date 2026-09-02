use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct PluginArgs {
    #[command(subcommand)]
    pub command: PluginCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PluginCommand {
    /// Inspect the normalized Agent Plugins package and its usable components.
    Inspect(PluginPathArgs),
    /// Validate an Agent Plugins package against the published 1.0 specification.
    Validate(PluginPathArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PluginPathArgs {
    /// Plugin root containing plugin.json.
    pub path: String,
    /// Persistent writable directory to project as PLUGIN_DATA.
    #[arg(long, value_name = "PATH")]
    pub data_dir: Option<String>,
    /// Emit the stable structured report as JSON.
    #[arg(long)]
    pub json: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;

    #[test]
    fn parses_structured_plugin_validation() {
        let cli = Cli::try_parse_from([
            "harn",
            "plugin",
            "validate",
            "./acme-tools",
            "--data-dir",
            "./state",
            "--json",
        ])
        .unwrap();
        let Some(Command::Plugin(PluginArgs {
            command: PluginCommand::Validate(args),
        })) = cli.command
        else {
            panic!("expected plugin validate command");
        };
        assert_eq!(args.path, "./acme-tools");
        assert_eq!(args.data_dir.as_deref(), Some("./state"));
        assert!(args.json);
    }
}

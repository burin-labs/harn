//! A versioned projection of the same Clap tree that parses `harn` invocations.

use clap::{Arg, ArgAction, Command, CommandFactory};
use serde::Serialize;

use crate::cli::Cli;
use crate::json_envelope::{to_string_pretty, JsonEnvelope};

const SCHEMA_VERSION: u32 = 1;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandSchema {
    name: String,
    aliases: Vec<String>,
    subcommand_required: bool,
    arguments: Vec<ArgumentSchema>,
    subcommands: Vec<CommandSchema>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ArgumentSchema {
    id: String,
    kind: &'static str,
    long: Option<String>,
    short: Option<char>,
    long_aliases: Vec<String>,
    short_aliases: Vec<char>,
    index: Option<usize>,
    required: bool,
    global: bool,
    trailing: bool,
    action: &'static str,
    min_values: usize,
    max_values: Option<usize>,
    value_names: Vec<String>,
    possible_values: Vec<String>,
}

pub(crate) fn run() {
    println!(
        "{}",
        to_string_pretty(&JsonEnvelope::ok(SCHEMA_VERSION, native_schema()))
    );
}

fn native_schema() -> CommandSchema {
    let mut command = Cli::command();
    command.build();
    project_command(&command)
}

fn project_command(command: &Command) -> CommandSchema {
    CommandSchema {
        name: command.get_name().to_owned(),
        aliases: command.get_all_aliases().map(str::to_owned).collect(),
        subcommand_required: command.is_subcommand_required_set(),
        arguments: command.get_arguments().map(project_argument).collect(),
        subcommands: command.get_subcommands().map(project_command).collect(),
    }
}

fn project_argument(argument: &Arg) -> ArgumentSchema {
    let range = argument.get_num_args();
    let action = argument.get_action();
    ArgumentSchema {
        id: argument.get_id().to_string(),
        kind: if argument.get_index().is_some() {
            "positional"
        } else if range.is_some_and(|range| range.takes_values()) {
            "option"
        } else {
            "flag"
        },
        long: argument.get_long().map(str::to_owned),
        short: argument.get_short(),
        long_aliases: argument
            .get_all_aliases()
            .unwrap_or_default()
            .into_iter()
            .map(str::to_owned)
            .collect(),
        short_aliases: argument.get_all_short_aliases().unwrap_or_default(),
        index: argument.get_index(),
        required: argument.is_required_set(),
        global: argument.is_global_set(),
        trailing: argument.is_trailing_var_arg_set(),
        action: match action {
            ArgAction::Set => "set",
            ArgAction::Append => "append",
            ArgAction::SetTrue => "setTrue",
            ArgAction::SetFalse => "setFalse",
            ArgAction::Count => "count",
            ArgAction::Help => "help",
            ArgAction::HelpShort => "helpShort",
            ArgAction::HelpLong => "helpLong",
            ArgAction::Version => "version",
            _ => "other",
        },
        min_values: range.map_or(0, |range| range.min_values()),
        max_values: range
            .and_then(|range| (range.max_values() != usize::MAX).then_some(range.max_values())),
        value_names: argument.get_value_names().map_or_else(Vec::new, |names| {
            names.iter().map(ToString::to_string).collect()
        }),
        possible_values: argument
            .get_possible_values()
            .iter()
            .map(|value| value.get_name().to_owned())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{error::ErrorKind, Parser};

    fn find<'a>(schema: &'a CommandSchema, path: &[&str]) -> &'a CommandSchema {
        path.iter().fold(schema, |parent, name| {
            parent
                .subcommands
                .iter()
                .find(|child| child.name == *name)
                .expect("native command")
        })
    }

    #[test]
    fn nested_native_flags_come_from_the_parser_and_reject_a_fake_flag() {
        let schema = native_schema();
        let ladder = find(&schema, &["merge-captain", "ladder"]);
        assert!(ladder
            .arguments
            .iter()
            .any(|arg| arg.long.as_deref() == Some("report-out") && arg.min_values == 1));
        assert!(ladder
            .arguments
            .iter()
            .any(|arg| arg.long.as_deref() == Some("format")
                && arg.possible_values.contains(&"json".to_owned())));
        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("merge-captain")
            .expect("parent")
            .find_subcommand_mut("ladder")
            .expect("child")
            .render_long_help()
            .to_string();
        assert!(help.contains("--report-out"));
        let error = Cli::try_parse_from([
            "harn",
            "merge-captain",
            "ladder",
            "manifest.toml",
            "--not-a-harn-flag",
        ])
        .expect_err("fake flag must fail");
        assert_eq!(error.kind(), ErrorKind::UnknownArgument);
    }

    #[test]
    fn schema_is_a_versioned_json_envelope() {
        let value =
            serde_json::to_value(JsonEnvelope::ok(SCHEMA_VERSION, native_schema())).unwrap();
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["ok"], true);
        assert!(value["data"]["subcommands"].as_array().unwrap().len() > 20);
    }
}

//! CLI argument-parsing tests, split by command area. Each submodule pulls the
//! shared imports below via `use super::*;`.

pub(crate) use clap::{CommandFactory, Parser};
pub(crate) use std::path::PathBuf;
pub(crate) use std::time::Duration as StdDuration;

use super::{
    Cli, Command, ConnectCommand, HostCommand, HostLeaseCommand, HostLeasePriorityArg,
    HostLeaseRunCommand, LocalCommand, MergeCaptainCommand, ModelsCommand, ModelsLoraCommand,
    OrchestratorCommand, OrchestratorQueueCommand, PersonaCommand, ProjectTemplate,
    ProviderCommand, ProviderPortableOptionArg, SessionCommand, SkillCommand, TimeCommand,
    ToolCommand, ToolSchemaSurface, TriggerCommand,
};
pub(crate) use crate::cli::runs::RunsCommand;

#[test]
fn clap_definition_is_valid() {
    Cli::command().debug_assert();
}

#[test]
fn environment_backed_argument_metadata_hides_values() {
    fn inspect(command: &clap::Command, unprotected: &mut Vec<String>) -> usize {
        let mut count = 0;
        for argument in command
            .get_arguments()
            .filter(|argument| argument.get_env().is_some())
        {
            count += 1;
            if !argument.is_hide_env_values_set() {
                unprotected.push(argument.get_id().to_string());
            }
        }
        for child in command.get_subcommands() {
            count += inspect(child, unprotected);
        }
        count
    }
    let control = clap::Command::new("fixture")
        .arg(clap::Arg::new("credential").env("CLI_TEST_DUMMY_CREDENTIAL"));
    let mut missing = Vec::new();
    assert_eq!(inspect(&control, &mut missing), 1);
    assert_eq!(missing, ["credential"]);
    let mut command = Cli::command();
    command.build();
    let mut missing = Vec::new();
    assert!(
        inspect(&command, &mut missing) >= 55,
        "must inspect the populated command tree"
    );
    assert!(
        missing.is_empty(),
        "env values visible in help: {missing:?}"
    );
}

mod parse_cmds;
mod parse_core;
mod parse_orchestration;
mod parse_provider_tool_calibrate;
mod parse_provider_tool_probe_audit;
mod parse_providers;
mod parse_runs;
mod parse_serve;

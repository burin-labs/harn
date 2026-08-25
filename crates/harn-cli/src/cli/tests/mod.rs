//! CLI argument-parsing tests, split by command area. Each submodule pulls the
//! shared imports below via `use super::*;`.

pub(crate) use clap::{CommandFactory, Parser};
pub(crate) use std::path::PathBuf;
pub(crate) use std::time::Duration as StdDuration;

use super::{
    Cli, Command, ConnectCommand, HostCommand, HostLeaseCommand, HostLeasePriorityArg,
    HostLeaseRunCommand, LocalCommand, MergeCaptainCommand, ModelsCommand, ModelsLoraCommand,
    OrchestratorCommand, OrchestratorQueueCommand, PersonaCommand, ProjectTemplate,
    ProviderCommand, SessionCommand, SkillCommand, TimeCommand, ToolCommand, TriggerCommand,
};
pub(crate) use crate::cli::runs::RunsCommand;

#[test]
fn clap_definition_is_valid() {
    Cli::command().debug_assert();
}

mod parse_cmds;
mod parse_core;
mod parse_orchestration;
mod parse_provider_tool_calibrate;
mod parse_provider_tool_probe_audit;
mod parse_providers;
mod parse_runs;
mod parse_serve;

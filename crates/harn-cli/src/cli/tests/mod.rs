//! CLI argument-parsing tests, split by command area. Each submodule pulls the
//! shared imports below via `use super::*;`.

pub(crate) use clap::{CommandFactory, Parser};
pub(crate) use std::path::PathBuf;
pub(crate) use std::time::Duration as StdDuration;

pub(crate) use super::{
    CheckOutputFormat, Cli, Command, CompletionShell, ConfigCommand, ConnectCommand,
    ConnectorCommand, CrystallizeCommand, EvalCommand, EvalToolCallsCommand, FlowArchivistCommand,
    FlowCommand, HarnessThreadingMode, LocalCommand, McpCommand, McpMockCommand,
    MergeCaptainCommand, ModelsCommand, ModelsLoraCommand, OrchestratorCommand,
    OrchestratorDeployProvider, OrchestratorLogFormat, OrchestratorQueueCommand,
    OrchestratorTenantCommand, PackageArtifactsCommand, PackageCacheCommand, PackageCommand,
    PackageScaffoldCommand, PersonaCommand, ProjectTemplate, ProviderCapabilitiesCommand,
    ProviderCatalogCommand, ProviderCommand, ProviderToolProbeModeArg, PublishArgs, RuleCommand,
    RunsCommand, SessionCommand, SkillCommand, SkillKeyCommand, SkillTrustCommand, ToolCommand,
    TraceCommand, TriggerCommand, TrustCommand, TrustOutcomeArg, TrustTierArg,
};

mod parse_cmds;
mod parse_core;
mod parse_orchestration;
mod parse_packaging;
mod parse_providers;
mod parse_serve;

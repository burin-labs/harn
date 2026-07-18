//! CLI argument-parsing tests, split by command area. Each submodule pulls the
//! shared imports below via `use super::*;`.

pub(crate) use clap::{CommandFactory, Parser};
pub(crate) use std::path::PathBuf;
pub(crate) use std::time::Duration as StdDuration;

pub(crate) use super::provider::{
    ProviderDispatchAuditVariantArg, ProviderToolProbeCaseArg, ProviderToolProbeModeArg,
};
pub(crate) use super::{
    CanonCommand, CheckOutputFormat, Cli, Command, CompletionShell, ConfigCommand, ConnectCommand,
    ConnectorCommand, CrystallizeCommand, EvalCommand, EvalToolCallsCommand, FlowArchivistCommand,
    FlowCommand, HarnessThreadingMode, HostCommand, HostLeaseCommand, HostLeasePriorityArg,
    HostLeaseResourceClassArg, HostLeaseRunCommand, LocalCommand, McpCommand, McpMockCommand,
    MergeCaptainCommand, ModelsBatchCommand, ModelsCommand, ModelsLoraCommand, OrchestratorCommand,
    OrchestratorDeployProvider, OrchestratorLogFormat, OrchestratorQueueCommand,
    OrchestratorTenantCommand, PackageArtifactsCommand, PackageCacheCommand, PackageCommand,
    PackageScaffoldCommand, PersonaCommand, ProjectTemplate, ProviderCapabilitiesCommand,
    ProviderCatalogCommand, ProviderCommand, PublishArgs, RuleCommand, RunsCommand, SessionCommand,
    SkillCommand, SkillKeyCommand, SkillTrustCommand, ToolCommand, TraceCommand, TriggerCommand,
    TrustCommand, TrustOutcomeArg, TrustTierArg,
};

mod parse_cmds;
mod parse_core;
mod parse_orchestration;
mod parse_packaging;
mod parse_provider_dispatch_audit;
mod parse_provider_tool_probe_audit;
mod parse_providers;
mod parse_serve;

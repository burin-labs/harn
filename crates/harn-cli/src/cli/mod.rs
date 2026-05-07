//! Top-level clap definition for the `harn` CLI.
//!
//! Per-subcommand arg structs live in their own modules under
//! `crates/harn-cli/src/cli/`. The `Cli` and `Command` enum here only
//! reference types via `pub(crate) use ...` re-exports so external
//! consumers can keep using `crate::cli::TypeName` unchanged.
//!
//! Some variants of subcommand enums are reached only by destructuring
//! the parent (`SubCommand::Foo(args) => ...`) and are never referenced
//! by name from outside this module. Those types stay private to their
//! per-subcommand module and are accessed through their parent enum.

mod bench;
mod check;
mod completions;
mod connect;
mod connector;
mod contracts;
mod crystallize;
mod doctor;
mod dump;
mod explain;
mod flow;
mod init;
mod lint_fmt;
mod mcp;
mod merge_captain;
mod models;
mod orchestrator;
mod package;
mod persona;
mod playground;
mod portal;
mod provider;
mod quickstart;
mod run;
mod runs;
mod serve;
mod skill;
mod skills;
mod test;
mod trace;
mod trigger;
mod trust;
mod try_cmd;
mod util;
mod verify;
mod viz;
mod watch;

pub(crate) use bench::BenchArgs;
pub(crate) use check::{CheckArgs, CheckOutputFormat};
pub(crate) use completions::{CompletionShell, CompletionsArgs};
pub(crate) use connect::{
    ConnectArgs, ConnectCommand, ConnectGenericArgs, ConnectGithubArgs, ConnectLinearArgs,
    ConnectOAuthArgs,
};
pub(crate) use connector::{
    ConnectorArgs, ConnectorCheckArgs, ConnectorCommand, ConnectorTestArgs,
};
pub(crate) use contracts::{
    ContractsArgs, ContractsBundleArgs, ContractsCommand, ContractsHostCapabilitiesArgs,
    ContractsOutputArgs,
};
pub(crate) use crystallize::{
    CrystallizeArgs, CrystallizeCommand, CrystallizeIngestArgs, CrystallizeShadowArgs,
    CrystallizeValidateArgs,
};
pub(crate) use doctor::DoctorArgs;
pub(crate) use dump::{
    DumpConnectorMatrixArgs, DumpHighlightKeywordsArgs, DumpTriggerQuickrefArgs,
};
pub(crate) use explain::ExplainArgs;
pub(crate) use flow::{
    FlowArchivistCommand, FlowArchivistScanArgs, FlowArgs, FlowCommand, FlowReplayAuditArgs,
    FlowShipCommand, FlowShipWatchArgs,
};
pub(crate) use init::{InitArgs, NewArgs, ProjectTemplate};
pub(crate) use lint_fmt::{FmtArgs, PathTargetsArgs};
pub(crate) use mcp::{McpArgs, McpCommand, McpLoginArgs, McpServeArgs, McpServerRefArgs};
pub(crate) use merge_captain::{
    MergeCaptainArgs, MergeCaptainAuditArgs, MergeCaptainAuditFormat, MergeCaptainBackendKind,
    MergeCaptainCommand, MergeCaptainIterateArgs, MergeCaptainIterateFormat,
    MergeCaptainLadderArgs, MergeCaptainLadderFormat, MergeCaptainMockCleanupArgs,
    MergeCaptainMockCommand, MergeCaptainMockInitArgs, MergeCaptainMockServeArgs,
    MergeCaptainMockStatusArgs, MergeCaptainMockStepArgs, MergeCaptainRunArgs,
};
pub(crate) use models::{
    ModelRecommendArgs, ModelsArgs, ModelsCommand, ModelsInstallArgs, ModelsListArgs,
};
pub(crate) use orchestrator::{
    OrchestratorArgs, OrchestratorCommand, OrchestratorDeployArgs, OrchestratorDeployProvider,
    OrchestratorDlqArgs, OrchestratorFireArgs, OrchestratorInspectArgs, OrchestratorLocalArgs,
    OrchestratorLogFormat, OrchestratorQueueArgs, OrchestratorQueueCommand,
    OrchestratorQueueDrainArgs, OrchestratorQueueLsArgs, OrchestratorQueuePurgeArgs,
    OrchestratorRecoverArgs, OrchestratorReloadArgs, OrchestratorReplayArgs,
    OrchestratorResumeArgs, OrchestratorServeArgs, OrchestratorStatsArgs, OrchestratorTenantArgs,
    OrchestratorTenantCommand, OrchestratorTenantCreateArgs, OrchestratorTenantDeleteArgs,
    OrchestratorTenantLsArgs, OrchestratorTenantSuspendArgs,
};
pub(crate) use package::{
    AddArgs, InstallArgs, PackageArgs, PackageCacheCommand, PackageCommand, PublishArgs,
    RemoveArgs, UpdateArgs,
};
pub(crate) use persona::{
    PersonaArgs, PersonaCheckArgs, PersonaCommand, PersonaControlArgs, PersonaDoctorArgs,
    PersonaInspectArgs, PersonaListArgs, PersonaNewArgs, PersonaSpendArgs, PersonaStatusArgs,
    PersonaTemplateKind, PersonaTickArgs, PersonaTriggerArgs,
};
pub(crate) use playground::PlaygroundArgs;
pub(crate) use portal::PortalArgs;
pub(crate) use provider::{ModelInfoArgs, ProviderCatalogArgs, ProviderReadyArgs};
pub(crate) use quickstart::QuickstartArgs;
pub(crate) use run::RunArgs;
pub(crate) use runs::{EvalArgs, ReplayArgs, RunsArgs, RunsCommand};
pub(crate) use serve::{
    A2aServeArgs, McpServeTransport, ServeAcpArgs, ServeArgs, ServeCommand, ServeMcpArgs,
    ServeTlsMode,
};
pub(crate) use skill::{
    SkillArgs, SkillCommand, SkillEndorseArgs, SkillKeyCommand, SkillKeyGenerateArgs,
    SkillSignArgs, SkillTrustAddArgs, SkillTrustCommand, SkillTrustListArgs, SkillVerifyArgs,
    SkillWhoSignedArgs,
};
pub(crate) use skills::{
    SkillsArgs, SkillsCommand, SkillsInspectArgs, SkillsInstallArgs, SkillsListArgs,
    SkillsMatchArgs, SkillsNewArgs,
};
pub(crate) use test::TestArgs;
pub(crate) use trace::{TraceArgs, TraceCommand, TraceImportArgs};
pub(crate) use trigger::{TriggerArgs, TriggerCancelArgs, TriggerCommand, TriggerReplayArgs};
pub(crate) use try_cmd::TryArgs;
// `TrustOutcomeArg` / `TrustTierArg` are referenced from the cli
// parser tests only; they're matched via destructuring elsewhere.
#[allow(unused_imports)]
pub(crate) use trust::{
    TrustArgs, TrustCommand, TrustExportArgs, TrustOutcomeArg, TrustQueryArgs, TrustTierArg,
    TrustVerifyChainArgs,
};
pub(crate) use verify::VerifyArgs;
pub(crate) use viz::VizArgs;
pub(crate) use watch::WatchArgs;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "harn",
    about = "The agent harness language",
    version,
    disable_help_subcommand = false,
    arg_required_else_help = true
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Execute a .harn file or an inline expression.
    #[command(long_about = "\
Execute a .harn file or an inline expression.

USAGE
    harn run script.harn
    harn run -e 'println(\"hello\")'
    harn run script.harn -- arg1 arg2   (script reads `argv` as list<string>)

CONCURRENCY
    Harn supports first-class concurrency primitives:
      - spawn { ... }         — launch a task, return a handle
      - parallel each LIST    — concurrent map
      - parallel settle LIST  — concurrent map, collect Ok/Err
      - parallel N            — N-way fan-out
      - with { max_concurrent: N }  — cap in-flight workers
      - channels, retry, select
    https://harnlang.com/concurrency.html

LLM THROTTLING
    Providers can be rate-limited via `rpm:` in harn.toml / providers.toml
    or via `HARN_RATE_LIMIT_<PROVIDER>=N`. Rate limits control throughput
    (RPM); `max_concurrent` on `parallel` caps simultaneous in-flight jobs.

SCRIPTING
    LLM-readable one-pager: https://harnlang.com/docs/llm/harn-quickref.html
    Human cheatsheet:       https://harnlang.com/scripting-cheatsheet.html
    Full docs:              https://harnlang.com/
")]
    Run(RunArgs),
    /// Type-check .harn files or directories without executing them.
    Check(CheckArgs),
    /// Explain the control-flow path that violates a Harn invariant
    /// (e.g. `fs.writes`, `approval.reachability`) for a specific
    /// function or trigger handler.
    Explain(ExplainArgs),
    /// Export machine-readable Harn contracts and bundle manifests.
    Contracts(ContractsArgs),
    /// Lint .harn files or directories for common issues.
    Lint(PathTargetsArgs),
    /// Format .harn files or directories.
    Fmt(FmtArgs),
    /// Run user tests or the conformance suite.
    Test(TestArgs),
    /// Scaffold a new project with harn.toml.
    Init(InitArgs),
    /// Scaffold a new project, package, or connector from a starter template.
    New(NewArgs),
    /// Diagnose the local Harn environment: toolchain version, configured
    /// LLM providers and credentials, MCP server reachability, file
    /// permissions on `~/.harn`, and project manifest health. Reports
    /// each check as ok/warn/fail with a suggested fix.
    Doctor(DoctorArgs),
    /// Configure a starter Harn project and LLM provider settings.
    Quickstart(QuickstartArgs),
    /// Register outbound connector resources with a provider.
    Connect(Box<ConnectArgs>),
    /// Validate pure-Harn connector packages against the connector contract.
    Connector(ConnectorArgs),
    /// Serve a Harn workflow over a transport adapter.
    Serve(ServeArgs),
    /// Manage remote MCP OAuth credentials and status.
    Mcp(McpArgs),
    /// Watch a .harn file and re-run it on changes.
    Watch(WatchArgs),
    /// Launch the local Harn observability portal.
    Portal(PortalArgs),
    /// Replay and inspect historical trigger dispatches from the event log.
    Trigger(TriggerArgs),
    /// Inspect Harn Flow atom, slice, and predicate audit state.
    Flow(FlowArgs),
    /// Import third-party eval traces into replayable Harn fixtures.
    Trace(TraceArgs),
    /// Mine repeated traces into a reviewable deterministic Harn workflow candidate.
    Crystallize(CrystallizeArgs),
    /// Query and manage trust-graph autonomy state.
    Trust(TrustArgs),
    /// Alias for `harn trust`. Query and verify trust-graph autonomy state.
    #[command(name = "trust-graph")]
    TrustGraph(TrustArgs),
    /// Verify a signed Harn provenance receipt.
    Verify(VerifyArgs),
    /// Print shell completion script to stdout.
    Completions(CompletionsArgs),
    /// Start the orchestrator process that hosts triggers and connector dispatch.
    Orchestrator(OrchestratorArgs),
    /// Run a pipeline against a Harn-native host module for fast iteration.
    Playground(PlaygroundArgs),
    /// Inspect persisted workflow run records.
    Runs(RunsArgs),
    /// Replay a persisted workflow run record.
    Replay(ReplayArgs),
    /// Evaluate a run record, run directory, or eval manifest.
    Eval(EvalArgs),
    /// Start the interactive REPL.
    Repl,
    /// Benchmark a .harn pipeline over repeated runs.
    Bench(BenchArgs),
    /// Render a .harn file as a Mermaid workflow graph.
    Viz(VizArgs),
    /// Install dependencies declared in harn.toml.
    Install(InstallArgs),
    /// Add a dependency to harn.toml.
    Add(AddArgs),
    /// Refresh one or more dependency lock entries.
    Update(UpdateArgs),
    /// Remove a dependency from harn.toml and harn.lock.
    Remove(RemoveArgs),
    /// Resolve dependencies and write harn.lock without materializing packages.
    Lock,
    /// Manage Harn package caches and integrity verification.
    Package(PackageArgs),
    /// Prepare a package manifest and bundle for publication. Validates
    /// `harn.toml` and produces a publishable archive locally; remote
    /// registry submission is not yet implemented (the resulting
    /// archive can be installed via `harn add <repo-or-path>` in the
    /// meantime).
    Publish(PublishArgs),
    /// List and inspect durable agent persona manifests.
    Persona(PersonaArgs),
    /// Merge Captain transcript oracle and audit (#1013).
    #[command(name = "merge-captain")]
    MergeCaptain(MergeCaptainArgs),
    /// Print resolved metadata for a model alias or model id as JSON.
    ModelInfo(ModelInfoArgs),
    /// Print the provider/model catalog Harn loaded as JSON.
    ProviderCatalog(ProviderCatalogArgs),
    /// Probe a provider's /models endpoint and optionally verify a served model.
    ProviderReady(ProviderReadyArgs),
    /// List or install LLM models. Convenience wrapper around the catalog
    /// and (for `install`) Ollama.
    Models(ModelsArgs),
    /// One-shot agent_loop with a prompt. Routes through the configured
    /// provider (or `HARN_LLM_PROVIDER=mock` for offline use).
    #[command(name = "try")]
    Try(TryArgs),
    /// Manage and inspect Harn skills (list, inspect, match, install, new).
    Skills(SkillsArgs),
    /// Manage skill provenance: keys, signatures, verification, and trust policy.
    Skill(SkillArgs),
    /// Print the decorated version banner.
    Version,
    /// Regenerate docs/theme/harn-keywords.js from the live lexer + stdlib sets.
    ///
    /// Dev-only. Hidden from `--help` — invoke via
    /// `cargo run -p harn-cli -- dump-highlight-keywords` or the
    /// `make gen-highlight` target.
    #[command(hide = true, name = "dump-highlight-keywords")]
    DumpHighlightKeywords(DumpHighlightKeywordsArgs),
    /// Regenerate docs/llm/harn-triggers-quickref.md from the live trigger provider catalog.
    ///
    /// Dev-only. Hidden from `--help` — invoke via
    /// `cargo run -p harn-cli -- dump-trigger-quickref` or the
    /// `make gen-trigger-quickref` target.
    #[command(hide = true, name = "dump-trigger-quickref")]
    DumpTriggerQuickref(DumpTriggerQuickrefArgs),
    /// Regenerate docs/src/connectors/parity-matrix.md from connector package manifests.
    ///
    /// Dev-only. Hidden from `--help` — invoke via
    /// `cargo run -p harn-cli -- dump-connector-matrix` or the
    /// `make gen-connector-matrix` target.
    #[command(hide = true, name = "dump-connector-matrix")]
    DumpConnectorMatrix(DumpConnectorMatrixArgs),
}

#[cfg(test)]
mod tests;

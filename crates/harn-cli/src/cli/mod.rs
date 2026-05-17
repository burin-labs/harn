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
mod config_cmd;
mod connect;
mod connector;
mod contracts;
mod crystallize;
mod demo;
mod doctor;
mod dump;
mod eval;
mod explain;
mod flow;
mod init;
mod lint_fmt;
mod local;
mod mcp;
mod merge_captain;
mod models;
mod orchestrator;
mod pack;
mod package;
mod persona;
mod playground;
mod portal;
mod precompile;
mod profile;
mod provider;
mod providers;
mod quickstart;
mod run;
mod runs;
mod serve;
mod session;
mod skill;
mod skills;
mod supervisor;
mod test;
mod test_bench;
mod time;
mod tool;
mod trace;
mod trigger;
mod trust;
mod try_cmd;
mod upgrade;
mod util;
mod verify;
mod viz;
mod watch;
mod workflow;

pub(crate) use bench::{BenchArgs, BenchCommand, BenchReplayArgs};
pub(crate) use check::{CheckArgs, CheckOutputFormat};
pub(crate) use completions::{CompletionShell, CompletionsArgs};
pub(crate) use config_cmd::{ConfigArgs, ConfigCommand, ConfigInspectArgs, ConfigValidateArgs};
pub(crate) use connect::{
    ConnectApiKeyArgs, ConnectArgs, ConnectCommand, ConnectGenericArgs, ConnectGithubArgs,
    ConnectLinearArgs, ConnectOAuthArgs, ConnectSetupPlanArgs, ConnectStatusArgs,
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
pub(crate) use demo::DemoArgs;
pub(crate) use doctor::DoctorArgs;
pub(crate) use dump::{
    DumpConnectorMatrixArgs, DumpHighlightKeywordsArgs, DumpProtocolArtifactsArgs,
    DumpTriggerQuickrefArgs,
};
pub use eval::{
    EvalArgs, EvalCommand, EvalPromptArgs, EvalPromptMode, EvalPromptOutput, EvalToolCallsArgs,
    EvalToolCallsCommand, EvalToolCallsRegressionArgs,
};
pub(crate) use explain::ExplainArgs;
pub(crate) use flow::{
    FlowArchivistCommand, FlowArchivistScanArgs, FlowArgs, FlowCommand, FlowReplayAuditArgs,
    FlowShipCommand, FlowShipWatchArgs,
};
pub(crate) use init::{InitArgs, NewArgs, ProjectTemplate};
pub(crate) use lint_fmt::{FmtArgs, PathTargetsArgs};
pub(crate) use local::{
    LocalArgs, LocalCommand, LocalListArgs, LocalProfileArgs, LocalStatusArgs, LocalStopArgs,
    LocalSwitchArgs,
};
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
    ModelsTestArgs,
};
pub(crate) use orchestrator::{
    OrchestratorArgs, OrchestratorCommand, OrchestratorDeployArgs, OrchestratorDeployProvider,
    OrchestratorDlqArgs, OrchestratorFireArgs, OrchestratorInspectArgs, OrchestratorLocalArgs,
    OrchestratorLogFormat, OrchestratorQueueArgs, OrchestratorQueueCommand,
    OrchestratorQueueDrainArgs, OrchestratorQueueLsArgs, OrchestratorQueuePurgeArgs,
    OrchestratorRecoverArgs, OrchestratorReloadArgs, OrchestratorReplayArgs,
    OrchestratorReplayOracleArgs, OrchestratorResumeArgs, OrchestratorServeArgs,
    OrchestratorStatsArgs, OrchestratorTenantArgs, OrchestratorTenantCommand,
    OrchestratorTenantCreateArgs, OrchestratorTenantDeleteArgs, OrchestratorTenantLsArgs,
    OrchestratorTenantSuspendArgs,
};
pub use pack::PackArgs;
pub(crate) use package::{
    AddArgs, InstallArgs, PackageArgs, PackageArtifactsCommand, PackageCacheCommand,
    PackageCommand, PublishArgs, RemoveArgs, UpdateArgs,
};
pub(crate) use persona::{
    PersonaArgs, PersonaCheckArgs, PersonaCommand, PersonaControlArgs, PersonaDoctorArgs,
    PersonaInspectArgs, PersonaListArgs, PersonaNewArgs, PersonaSpendArgs, PersonaStatusArgs,
    PersonaSupervisionCommand, PersonaSupervisionTailArgs, PersonaTemplateKind, PersonaTickArgs,
    PersonaTriggerArgs,
};
pub(crate) use playground::PlaygroundArgs;
pub(crate) use portal::PortalArgs;
pub use precompile::PrecompileArgs;
pub(crate) use profile::ProfileArgs;
pub(crate) use provider::{
    ModelInfoArgs, ProviderCatalogArgs, ProviderProbeArgs, ProviderReadyArgs,
    ProviderToolProbeArgs, ProviderToolProbeModeArg,
};
pub(crate) use providers::{
    ProvidersArgs, ProvidersCommand, ProvidersExportArgs, ProvidersRefreshArgs,
    ProvidersValidateArgs,
};
pub(crate) use quickstart::QuickstartArgs;
pub(crate) use run::RunArgs;
pub(crate) use runs::{ReplayArgs, RunsArgs, RunsCommand};
pub(crate) use serve::{
    A2aServeArgs, ApiServeArgs, McpServeTransport, ServeAcpArgs, ServeArgs, ServeCommand,
    ServeMcpArgs, ServeTlsMode,
};
pub(crate) use session::{
    SessionArgs, SessionCommand, SessionExportArgs, SessionImportArgs, SessionSchemaArgs,
    SessionValidateArgs,
};
pub(crate) use skill::{
    SkillArgs, SkillCommand, SkillEndorseArgs, SkillKeyCommand, SkillKeyGenerateArgs,
    SkillSignArgs, SkillTrustAddArgs, SkillTrustCommand, SkillTrustListArgs, SkillVerifyArgs,
    SkillWhoSignedArgs,
};
pub(crate) use skills::{
    SkillsArgs, SkillsCommand, SkillsDumpArgs, SkillsGetArgs, SkillsInspectArgs, SkillsInstallArgs,
    SkillsListArgs, SkillsMatchArgs, SkillsNewArgs, SkillsResolvedArgs,
};
pub(crate) use supervisor::{
    SupervisorArgs, SupervisorCommand, SupervisorDlqCommand, SupervisorDlqListArgs,
    SupervisorDlqReplayArgs, SupervisorFireArgs, SupervisorInspectArgs, SupervisorListArgs,
    SupervisorPauseArgs, SupervisorRecoverArgs, SupervisorReplayArgs, SupervisorResumeArgs,
    SupervisorStartArgs, SupervisorStopArgs,
};
pub(crate) use test::TestArgs;
pub(crate) use test_bench::{
    TestBenchArgs, TestBenchCommand, TestBenchExportAnnotationsArgs, TestBenchFidelityArgs,
    TestBenchReplayArgs, TestBenchRunArgs, TestBenchValidateAnnotationsArgs,
};
pub(crate) use time::{TimeArgs, TimeCommand, TimeRunArgs};
pub(crate) use tool::{ToolArgs, ToolCommand, ToolNewArgs};
pub(crate) use trace::{TraceArgs, TraceCommand, TraceImportArgs};
pub(crate) use trigger::{TriggerArgs, TriggerCancelArgs, TriggerCommand, TriggerReplayArgs};
pub(crate) use try_cmd::TryArgs;
pub(crate) use upgrade::UpgradeArgs;
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
pub(crate) use workflow::{
    WorkflowArgs, WorkflowCommand, WorkflowFunctionToolsArgs, WorkflowNestedCeilingArgs,
    WorkflowPatchApplyArgs, WorkflowPatchCommand, WorkflowPatchPreviewArgs,
    WorkflowPatchValidateArgs,
};

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
    /// Emit the JSON-schema catalog for every `harn` subcommand that
    /// exposes a structured `--json` envelope. Pair with
    /// `--command <name>` to print just one entry.
    #[arg(long = "json-schemas", global = false)]
    pub json_schemas: bool,

    /// When combined with `--json-schemas`, restrict the catalog to a
    /// single command name (e.g. `--command run`).
    #[arg(
        long = "command",
        requires = "json_schemas",
        value_name = "COMMAND",
        global = false
    )]
    pub schema_command: Option<String>,

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
    /// Inspect, validate, and emit schemas for layered Harn runtime config.
    Config(ConfigArgs),
    /// Explain a diagnostic. Pass a stable `HARN-<CAT>-<NNN>` code
    /// (optionally with `--json` for the structured envelope), or the
    /// legacy `--invariant <NAME> <FUNCTION> <FILE>` form to walk the
    /// control-flow path behind a Harn invariant violation.
    Explain(ExplainArgs),
    /// Export machine-readable Harn contracts and bundle manifests.
    Contracts(ContractsArgs),
    /// Lint .harn files or directories for common issues.
    Lint(PathTargetsArgs),
    /// Format .harn files or directories.
    Fmt(FmtArgs),
    /// Run user tests or the conformance suite.
    Test(TestArgs),
    /// Run a .harn script under a hermetic testbench (paused clock,
    /// optional LLM/process tapes, fs overlay, deny-by-default network).
    #[command(name = "test-bench")]
    TestBench(TestBenchArgs),
    /// Instrument a wrapped subcommand with phase-level wall-clock
    /// timing (parse, typecheck, bytecode compile + cache hit/miss,
    /// run setup, run main) plus per-LLM-call and per-tool-call
    /// latency. Pair with `--json` for an agent-readable envelope.
    Time(TimeArgs),
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
    /// Run a bundled offline demo scenario to see Harn in action without
    /// API keys. `harn demo` lists scenarios; `harn demo <id>` runs one.
    Demo(DemoArgs),
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
    /// Validate, preview, and run portable workflow bundles.
    Workflow(WorkflowArgs),
    /// Control local durable workflow automations for trusted hosts.
    Supervisor(SupervisorArgs),
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
    /// Export, import, and validate portable Harn session bundles.
    Session(SessionArgs),
    /// Replay a persisted workflow run record.
    Replay(ReplayArgs),
    /// Evaluate a run record, run directory, or eval manifest.
    Eval(EvalArgs),
    /// Start the interactive REPL.
    Repl,
    /// Benchmark a .harn pipeline over repeated runs.
    Bench(BenchArgs),
    /// Pre-compile `.harn` sources into the content-addressed bytecode
    /// cache so cold-start `harn run` for the same source skips parse
    /// and compile and goes straight to bytecode load.
    ///
    /// `harn precompile path/` walks the directory and compiles every
    /// `.harn` file; `harn precompile script.harn` compiles a single
    /// file. Artifacts are written adjacent to each source as
    /// `<name>.harnbc` by default; pass `--out DIR` to redirect them
    /// into a sibling tree.
    Precompile(PrecompileArgs),
    /// Build a signed-ready `.harnpack` run bundle from a Harn entrypoint.
    ///
    /// `harn pack <entrypoint>` walks the entrypoint's transitive imports,
    /// precompiles every module, snapshots the provider catalog and
    /// stdlib pin, generates a minimal SBOM, and emits a deterministic
    /// tar.zst container under `<entrypoint>.harnpack` (or `--out`).
    ///
    /// `--upgrade <old.harnpack>` reads an existing bundle (v1 or v2) and
    /// re-emits it under the v2 manifest, preserving the prior bundle's
    /// workflow graph, triggers, and prompt capsules.
    ///
    /// Signing (E6.3) and richer SBOM enumeration (E6.4) land in
    /// follow-ups; today the bundle ships unsigned with a stdlib-pin SBOM.
    Pack(PackArgs),
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
    /// List, install, recommend, and test configured LLM models.
    Models(ModelsArgs),
    /// Manage local LLM runtime lifecycle: enumerate, switch, and stop
    /// Ollama, llama.cpp, MLX, and other OpenAI-compatible local servers.
    Local(LocalArgs),
    /// Validate and generate provider/model catalog artifacts.
    Providers(ProvidersArgs),
    /// Print the provider/model catalog Harn loaded as JSON.
    ProviderCatalog(ProviderCatalogArgs),
    /// Probe a provider's /models endpoint and optionally verify a served model.
    ProviderReady(ProviderReadyArgs),
    /// Snapshot a provider: readiness, served models, loaded models with
    /// memory/context details. Designed for eval pipelines that need a
    /// stable telemetry envelope per provider.
    ProviderProbe(ProviderProbeArgs),
    /// Run one-tool provider conformance and classify native/text fallback.
    ProviderToolProbe(ProviderToolProbeArgs),
    /// One-shot agent_loop with a prompt. Routes through the configured
    /// provider (or `HARN_LLM_PROVIDER=mock` for offline use).
    #[command(name = "try")]
    Try(TryArgs),
    /// Manage and inspect Harn skills (list/get/dump for the embedded
    /// corpus; resolved/inspect/match/install/new for FS-resolved skills).
    Skills(SkillsArgs),
    /// Manage skill provenance: keys, signatures, verification, and trust policy.
    Skill(SkillArgs),
    /// Scaffold and inspect Harn-native custom tools.
    Tool(ToolArgs),
    /// Print the decorated version banner.
    Version,
    /// Download and atomically replace the running `harn` binary with
    /// the latest published GitHub release (or a specific tag via
    /// `--version`). Verifies the archive against the release's
    /// `SHA256SUMS` manifest before installing.
    Upgrade(UpgradeArgs),
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
    /// Regenerate Harn protocol schemas and TypeScript/Swift bindings.
    ///
    /// Dev-only. Hidden from `--help` — invoke via
    /// `cargo run -p harn-cli -- dump-protocol-artifacts` or the
    /// `make gen-protocol-artifacts` target.
    #[command(hide = true, name = "dump-protocol-artifacts")]
    DumpProtocolArtifacts(DumpProtocolArtifactsArgs),
}

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use clap::{ArgAction, Args, Subcommand};

use super::util::{llm_model_completion_parser, llm_provider_completion_parser};

#[derive(Debug, Args)]
pub(crate) struct ProviderArgs {
    #[command(subcommand)]
    pub command: ProviderCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ProviderCommand {
    /// Inspect provider/model capability metadata.
    Capabilities(ProviderCapabilitiesArgs),
    /// Validate and generate provider/model catalog artifacts, or print the
    /// catalog Harn loaded (`provider catalog show`).
    Catalog(super::providers::ProviderCatalogArgs),
    /// Probe a provider's /models endpoint and optionally verify a served model.
    Ready(ProviderReadyArgs),
    /// Snapshot a provider: readiness, served models, loaded models with
    /// memory/context details. Designed for eval pipelines that need a
    /// stable telemetry envelope per provider.
    Probe(ProviderProbeArgs),
    /// Run one-tool provider conformance and classify native/text fallback.
    ToolProbe(ProviderToolProbeArgs),
    /// Aggregate saved tool-probe reports into a provider/model scorecard.
    ToolScorecard(ProviderToolScorecardArgs),
    /// Classify prompt-cache conformance from a saved repeat-run usage fixture:
    /// resolve capability support, normalize each run's usage, and emit one
    /// cache verdict. Live repeat probing is not yet wired; pass
    /// `--usage-fixture`.
    CacheProbe(ProviderCacheProbeArgs),
    /// Deterministically explain how a (provider, model) pair would dispatch:
    /// resolved wire format (anthropic-native vs openai-compat), base URL host,
    /// native-tool support, and thinking eligibility — a pure capability-registry
    /// lookup with NO network call and NO LLM call. Answers "does anthropic
    /// claude-sonnet route native?" instantly, without running an eval.
    #[command(name = "dispatch-explain")]
    DispatchExplain(ProviderDispatchExplainArgs),
    /// Report the LLM rate/concurrency governor's live state: the resolved
    /// per-provider limits from the catalog, and — when the `llm.rate_governor`
    /// flag is on and calls have flowed — each (provider, org_key)'s AIMD
    /// concurrency limit, circuit state, in-flight count, and last throttle
    /// signal. Deterministic, no network, no LLM. The one-call answer to "is a
    /// provider being throttled right now, and how is the governor reacting?" —
    /// the sibling of `dispatch-explain`.
    Limits(ProviderLimitsArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ProviderLimitsArgs {
    /// Restrict the report to a single provider id (e.g. `anthropic`). Omit to
    /// report every provider with a catalog limit row plus every live governor.
    #[arg(
        value_parser = llm_provider_completion_parser(),
        hide_possible_values = true
    )]
    pub provider: Option<String>,
    /// Emit the structured report as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ProviderDispatchExplainArgs {
    /// Provider id (e.g. `anthropic`, `openrouter`, `openai`).
    #[arg(
        value_parser = llm_provider_completion_parser(),
        hide_possible_values = true
    )]
    pub provider: String,
    /// Model alias or provider-native model id (e.g. `claude-sonnet-4-6`).
    #[arg(
        value_parser = llm_model_completion_parser(),
        hide_possible_values = true
    )]
    pub model: String,
    /// Report as if extended thinking were requested. Affects the thinking
    /// eligibility line; does not change wire-format resolution.
    #[arg(long)]
    pub thinking: bool,
    /// Explain a specific tool_format (`native` / `text` / `json`) instead of
    /// the model's preferred format.
    #[arg(long = "tool-format")]
    pub tool_format: Option<String>,
    /// Emit the structured explanation as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ProviderCapabilitiesArgs {
    #[command(subcommand)]
    pub command: ProviderCapabilitiesCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ProviderCapabilitiesCommand {
    /// Audit catalogued priced chat models for explicit tool capability fields.
    Audit(ProviderCapabilitiesAuditArgs),
    /// Apply a generated parity overlay to the capability catalog.
    PromoteFromEval(ProviderCapabilitiesPromoteFromEvalArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ProviderCapabilitiesAuditArgs {
    /// Emit the structured audit report as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ProviderCapabilitiesPromoteFromEvalArgs {
    /// Generated overlay TOML from `harn eval coding-agent`.
    pub overlay_path: PathBuf,
    /// Capability catalog TOML to update in place.
    #[arg(long, default_value = "crates/harn-vm/src/llm/capabilities.toml")]
    pub catalog: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct ProviderCatalogShowArgs {
    /// Only include providers that are usable in the current environment.
    #[arg(long)]
    pub available_only: bool,
    /// Refresh the runtime provider catalog overlay before printing.
    #[arg(long)]
    pub refresh: bool,
}

pub(crate) async fn refresh_provider_catalog_if_requested(args: &ProviderCatalogShowArgs) {
    if !args.refresh {
        return;
    }
    let report = harn_vm::provider_catalog::refresh_runtime_catalog(
        harn_vm::provider_catalog::CatalogRefreshOptions {
            url: None,
            force: false,
        },
    )
    .await;
    if let Some(warning) = report.warning.as_deref() {
        eprintln!(
            "warning: provider catalog refresh {}: {warning}",
            report.status
        );
    }
}

#[derive(Debug, Args)]
pub(crate) struct ProviderReadyArgs {
    /// Provider id from Harn provider config, for example mlx or local.
    #[arg(
        value_parser = llm_provider_completion_parser(),
        hide_possible_values = true
    )]
    pub provider: String,
    /// Model alias or provider-native model id to require in /models.
    #[arg(
        long,
        value_parser = llm_model_completion_parser(),
        hide_possible_values = true
    )]
    pub model: Option<String>,
    /// Override the configured provider base URL for this probe.
    #[arg(long = "base-url")]
    pub base_url: Option<String>,
    /// Emit the full structured readiness result as JSON.
    #[arg(long, default_value_t = false, action = ArgAction::SetTrue)]
    pub json: bool,
}

/// Surface for `harn provider probe`: combined `/v1/models` readiness +
/// loaded-model state (`/api/ps` for Ollama) under one machine-readable
/// command. Evals consume the JSON to record cold load time / VRAM /
/// context length alongside per-call telemetry.
#[derive(Debug, Args)]
pub(crate) struct ProviderProbeArgs {
    /// Provider id from Harn provider config (`ollama`, `llamacpp`, `mlx`,
    /// `openai`, ...). Required because the probe is provider-scoped.
    #[arg(
        value_parser = llm_provider_completion_parser(),
        hide_possible_values = true
    )]
    pub provider: String,
    /// Optional model alias or provider-native id. When set the probe
    /// also confirms the model is currently served.
    #[arg(
        long,
        value_parser = llm_model_completion_parser(),
        hide_possible_values = true
    )]
    pub model: Option<String>,
    /// Override the configured provider base URL.
    #[arg(long = "base-url")]
    pub base_url: Option<String>,
    /// Emit JSON. Defaults to true since this command is meant for
    /// machine consumption (eval aggregators); pass `--json=false` to
    /// drop back to the human summary the readiness probe prints.
    #[arg(
        long,
        default_value_t = true,
        num_args = 0..=1,
        default_missing_value = "true",
        action = ArgAction::Set
    )]
    pub json: bool,
}

/// Run the one-tool provider conformance probe and emit JSON that eval
/// harnesses can use to select native, text, or disabled tool mode.
#[derive(Debug, Args)]
pub(crate) struct ProviderToolProbeArgs {
    /// Provider id from Harn provider config (`ollama`, `llamacpp`, `mlx`,
    /// `local`, ...).
    #[arg(
        value_parser = llm_provider_completion_parser(),
        hide_possible_values = true
    )]
    pub provider: String,
    /// Model alias or provider-native model id.
    #[arg(
        long,
        value_parser = llm_model_completion_parser(),
        hide_possible_values = true
    )]
    pub model: String,
    /// Override the configured provider base URL.
    #[arg(long = "base-url")]
    pub base_url: Option<String>,
    /// Probe only one transport mode instead of both.
    #[arg(long, value_enum, default_value_t = ProviderToolProbeModeArg::Both)]
    pub mode: ProviderToolProbeModeArg,
    /// Override the marker the model must echo through the tool call.
    #[arg(long, default_value = harn_vm::llm::tool_conformance::DEFAULT_TOOL_PROBE_MARKER)]
    pub marker: String,
    /// Repeat each live probe mode N times.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..=100))]
    pub repeat: u16,
    /// Classify a saved provider response body instead of making a live request.
    #[arg(long = "response-fixture")]
    pub response_fixture: Option<PathBuf>,
    /// Request timeout in seconds for each live probe case.
    #[arg(long, default_value_t = 120)]
    pub timeout_secs: u64,
    /// Emit JSON. Defaults to true because evals and setup scripts consume
    /// the structured conformance report.
    #[arg(
        long,
        default_value_t = true,
        num_args = 0..=1,
        default_missing_value = "true",
        action = ArgAction::Set
    )]
    pub json: bool,
}

/// Aggregate saved `harn provider tool-probe` JSON reports into one
/// deterministic provider/model tool-call quality scorecard. This first slice
/// is fixture-only by design: it never calls providers and never mutates the
/// catalog.
#[derive(Debug, Args)]
pub(crate) struct ProviderToolScorecardArgs {
    /// Saved JSON output from `harn provider tool-probe`. Repeat the flag to
    /// aggregate several routes into one scorecard.
    #[arg(long = "tool-probe-report", required = true)]
    pub tool_probe_reports: Vec<PathBuf>,
    /// Emit JSON. Defaults to true because catalog reviews and promotion gates
    /// consume the structured scorecard.
    #[arg(
        long,
        default_value_t = true,
        num_args = 0..=1,
        default_missing_value = "true",
        action = ArgAction::Set
    )]
    pub json: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum ProviderToolProbeModeArg {
    Both,
    NonStreaming,
    Streaming,
}

/// Surface for `harn provider cache-probe`: classify a saved repeat-run
/// prompt-cache usage fixture into one cache verdict. The classifier is the
/// stable contract Burin dogfood and Harn Cloud receipts consume; live probing
/// is deliberately fixture-first so committed conformance carries no keys.
#[derive(Debug, Args)]
pub(crate) struct ProviderCacheProbeArgs {
    /// Provider id from Harn provider config. Optional when the fixture object
    /// carries its own `provider`.
    #[arg(
        value_parser = llm_provider_completion_parser(),
        hide_possible_values = true,
        default_value = ""
    )]
    pub provider: String,
    /// Model alias or provider-native model id. Optional when the fixture object
    /// carries its own `model`.
    #[arg(
        long,
        value_parser = llm_model_completion_parser(),
        hide_possible_values = true,
        default_value = ""
    )]
    pub model: String,
    /// Saved repeat-run usage fixture: a JSON runs array, or an object with a
    /// `runs` array plus optional `provider`/`model`.
    #[arg(long = "usage-fixture")]
    pub usage_fixture: Option<PathBuf>,
    /// Emit JSON. Defaults to true because evals and dogfood gates consume the
    /// structured conformance report.
    #[arg(
        long,
        default_value_t = true,
        num_args = 0..=1,
        default_missing_value = "true",
        action = ArgAction::Set
    )]
    pub json: bool,
}

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
    /// Render and validate every catalogued provider tool-probe request shape
    /// without calling providers.
    #[command(name = "tool-probe-audit")]
    ToolProbeAudit(ProviderToolProbeAuditArgs),
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
    /// Select the fixed micro-case to probe.
    #[arg(long = "case", value_enum, default_value_t = ProviderToolProbeCaseArg::SingleToolCall)]
    pub probe_case: ProviderToolProbeCaseArg,
    /// Select the provider request-profile to render for --dry-run-request.
    /// Non-default profiles are offline request-audit surfaces, not live
    /// conformance probes.
    #[arg(long = "request-profile", value_enum, default_value_t = ProviderToolProbeRequestProfileArg::CatalogDefault)]
    pub request_profile: ProviderToolProbeRequestProfileArg,
    /// Override the marker, or marker seed for structured multi-line cases.
    #[arg(long, default_value = harn_vm::llm::tool_conformance::DEFAULT_TOOL_PROBE_MARKER)]
    pub marker: String,
    /// Repeat each live probe mode N times.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..=100))]
    pub repeat: u16,
    /// Classify a saved provider response body instead of making a live request.
    #[arg(long = "response-fixture", conflicts_with = "dry_run_request")]
    pub response_fixture: Option<PathBuf>,
    /// Print the provider-compatible request body and exit without calling the
    /// provider. Useful for catalog/request-shape audits during cooldowns.
    #[arg(long = "dry-run-request")]
    pub dry_run_request: bool,
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

/// Offline full-catalog request-shape audit for tool probes.
#[derive(Debug, Args)]
pub(crate) struct ProviderToolProbeAuditArgs {
    /// Probe only one transport mode instead of both.
    #[arg(long, value_enum, default_value_t = ProviderToolProbeModeArg::Both)]
    pub mode: ProviderToolProbeModeArg,
    /// Restrict the audit to one or more fixed micro-cases. Omit to run every
    /// request-rendered case.
    #[arg(long = "case", value_enum)]
    pub probe_cases: Vec<ProviderToolProbeCaseArg>,
    /// Restrict the audit to one or more request profiles. Omit to run every
    /// offline request profile.
    #[arg(long = "request-profile", value_enum)]
    pub request_profiles: Vec<ProviderToolProbeRequestProfileArg>,
    /// Emit JSON. Defaults to true because CI and catalog reviews consume the
    /// structured audit report.
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
    /// Emit the catalog-backed fixed micro-case plan instead of aggregating
    /// saved tool-probe reports.
    #[arg(long = "plan-from-catalog", conflicts_with = "tool_probe_reports")]
    pub plan_from_catalog: bool,
    /// Restrict `--plan-from-catalog` to one catalog route. Repeat for
    /// multiple routes.
    /// Format is `provider:model`; the model part may contain additional `:`
    /// characters.
    #[arg(long = "route", requires = "plan_from_catalog")]
    pub routes: Vec<String>,
    /// Include batch-manifest request rows for latency-tolerant single-turn
    /// cases on routes whose catalog/capabilities claim batch support.
    #[arg(long, requires = "plan_from_catalog")]
    pub include_batch_manifest: bool,
    /// Emit a Markdown artifact instead of JSON or the compact human summary.
    #[arg(long, conflicts_with = "json")]
    pub markdown: bool,
    /// Saved JSON output from `harn provider tool-probe`. Repeat the flag to
    /// aggregate several routes into one scorecard.
    #[arg(
        long = "tool-probe-report",
        required_unless_present = "plan_from_catalog"
    )]
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

impl ProviderToolProbeModeArg {
    pub(crate) fn tool_probe_modes(self) -> Vec<harn_vm::llm::tool_conformance::ToolProbeMode> {
        use harn_vm::llm::tool_conformance::ToolProbeMode;
        match self {
            Self::Both => vec![ToolProbeMode::NonStreaming, ToolProbeMode::Streaming],
            Self::NonStreaming => vec![ToolProbeMode::NonStreaming],
            Self::Streaming => vec![ToolProbeMode::Streaming],
        }
    }
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum ProviderToolProbeCaseArg {
    #[value(name = "single_tool_call", alias = "single-tool-call")]
    SingleToolCall,
    #[value(name = "large_string_argument", alias = "large-string-argument")]
    LargeStringArgument,
    #[value(
        name = "no_tool_answer_or_refusal",
        alias = "no-tool-answer-or-refusal"
    )]
    NoToolAnswerOrRefusal,
    #[value(name = "unavailable_tool_repair", alias = "unavailable-tool-repair")]
    UnavailableToolRepair,
    #[value(name = "done_sentinel", alias = "done-sentinel")]
    DoneSentinel,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum, PartialEq, Eq)]
pub(crate) enum ProviderToolProbeRequestProfileArg {
    #[value(name = "catalog_default", alias = "catalog-default")]
    CatalogDefault,
    #[value(name = "parameter_edges", alias = "parameter-edges")]
    ParameterEdges,
}

impl ProviderToolProbeRequestProfileArg {
    pub(crate) fn tool_probe_request_profile(
        self,
    ) -> harn_vm::llm::tool_conformance::ToolProbeRequestProfile {
        match self {
            Self::CatalogDefault => {
                harn_vm::llm::tool_conformance::ToolProbeRequestProfile::CatalogDefault
            }
            Self::ParameterEdges => {
                harn_vm::llm::tool_conformance::ToolProbeRequestProfile::ParameterEdges
            }
        }
    }
}

impl ProviderToolProbeCaseArg {
    pub(crate) fn tool_probe_case(self) -> harn_vm::llm::tool_conformance::ToolProbeCase {
        match self {
            Self::SingleToolCall => harn_vm::llm::tool_conformance::ToolProbeCase::SingleToolCall,
            Self::LargeStringArgument => {
                harn_vm::llm::tool_conformance::ToolProbeCase::LargeStringArgument
            }
            Self::NoToolAnswerOrRefusal => {
                harn_vm::llm::tool_conformance::ToolProbeCase::NoToolAnswerOrRefusal
            }
            Self::UnavailableToolRepair => {
                harn_vm::llm::tool_conformance::ToolProbeCase::UnavailableToolRepair
            }
            Self::DoneSentinel => harn_vm::llm::tool_conformance::ToolProbeCase::DoneSentinel,
        }
    }
}

impl ProviderToolProbeArgs {
    pub(crate) fn live_request_profile_error(&self) -> Option<&'static str> {
        if self.dry_run_request
            || self.request_profile == ProviderToolProbeRequestProfileArg::CatalogDefault
        {
            None
        } else {
            Some(
                "error: --request-profile is only supported with --dry-run-request; \
                 live tool probes use catalog_default",
            )
        }
    }

    pub(crate) fn tool_conformance_probe_options(
        &self,
    ) -> harn_vm::llm::tool_conformance::ToolConformanceProbeOptions {
        let mut options = harn_vm::llm::tool_conformance::ToolConformanceProbeOptions::new(
            self.provider.clone(),
            self.model.clone(),
        );
        options.base_url = self.base_url.clone();
        options.modes = self.mode.tool_probe_modes();
        options.probe_case = self.probe_case.tool_probe_case();
        options.marker = self.marker.clone();
        options.repeat = usize::from(self.repeat);
        options.timeout_secs = self.timeout_secs;
        options
    }
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

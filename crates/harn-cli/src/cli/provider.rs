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

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum ProviderToolProbeModeArg {
    Both,
    NonStreaming,
    Streaming,
}

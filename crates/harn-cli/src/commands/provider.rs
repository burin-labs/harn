//! `harn provider probe` — one-shot machine-readable provider snapshot.
//!
//! Combines provider readiness (`/v1/models` or equivalent) with the
//! runtime-state details that local engines surface separately (Ollama's
//! `/api/ps` shows VRAM, size, expiry, and on newer builds the context
//! window the model was loaded with). Output is a structured envelope so
//! eval pipelines decode it with the same shape they use for per-call
//! `provider_telemetry`.
//!
//! ## .harn dispatch
//!
//! Aggregation stays in Rust (sandboxed scripts can't run the
//! readiness HTTP probe + `/api/ps` calls), the rendering layer lives
//! in `crates/harn-stdlib/src/stdlib/cli/providers/{probe,tool_probe}.harn`.

use std::io::Write as _;
use std::process;

use harn_vm::llm::readiness::{probe_provider_readiness, ProviderReadiness};
use harn_vm::llm_config;
use serde::Serialize;

use crate::cli::{
    ProviderCacheProbeArgs, ProviderProbeArgs, ProviderToolProbeArgs, ProviderToolProbeModeArg,
};
use crate::commands::local::runtime::{fetch_ollama_ps, LoadedModel, LOCAL_PROVIDERS};
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

/// Env var carrying the JSON `ProviderProbe` envelope handed across to
/// the embedded `cli/providers/probe` script.
const PROBE_PAYLOAD_ENV: &str = "HARN_PROVIDER_PROBE_PAYLOAD_JSON";

/// Pretty-printed companion to [`PROBE_PAYLOAD_ENV`]. The script
/// forwards these bytes directly in `--json` mode so Harn's
/// `json_parse`/`json_stringify` round-trip can't normalise float
/// fields like pricing entries that serde keeps typed.
const PROBE_PAYLOAD_PRETTY_ENV: &str = "HARN_PROVIDER_PROBE_PAYLOAD_PRETTY";

/// Env var carrying the JSON `ToolConformanceReport` envelope handed
/// across to the embedded `cli/providers/tool_probe` script.
const TOOL_PROBE_PAYLOAD_ENV: &str = "HARN_PROVIDER_TOOL_PROBE_PAYLOAD_JSON";

/// Pretty-printed companion to [`TOOL_PROBE_PAYLOAD_ENV`] — same
/// rationale as [`PROBE_PAYLOAD_PRETTY_ENV`].
const TOOL_PROBE_PAYLOAD_PRETTY_ENV: &str = "HARN_PROVIDER_TOOL_PROBE_PAYLOAD_PRETTY";

/// Env var carrying the JSON `CacheConformanceReport` envelope handed across
/// to the embedded `cli/providers/cache_probe` render script.
const CACHE_PROBE_PAYLOAD_ENV: &str = "HARN_PROVIDER_CACHE_PROBE_PAYLOAD_JSON";

/// Pretty-printed companion to [`CACHE_PROBE_PAYLOAD_ENV`] — same rationale as
/// [`PROBE_PAYLOAD_PRETTY_ENV`].
const CACHE_PROBE_PAYLOAD_PRETTY_ENV: &str = "HARN_PROVIDER_CACHE_PROBE_PAYLOAD_PRETTY";

/// Serialises the dispatch path so concurrent in-process callers
/// don't race on the global env vars the shim sets.
static DISPATCH_PROBE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static DISPATCH_TOOL_PROBE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static DISPATCH_CACHE_PROBE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Serialize)]
struct ProviderProbe {
    provider: String,
    base_url: Option<String>,
    readiness: ProviderReadiness,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_profile: Option<harn_vm::llm::local_profiles::LocalRuntimeProfileReport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    loaded_models: Vec<LoadedModel>,
}

pub(crate) async fn run_provider_probe(args: ProviderProbeArgs) {
    let exit_code = dispatch_provider_probe(args).await;
    if exit_code != 0 {
        process::exit(exit_code);
    }
}

async fn dispatch_provider_probe(args: ProviderProbeArgs) -> i32 {
    let probe = aggregate_provider_probe(&args).await;
    let exit_code = i32::from(!probe.readiness.ok);
    let payload_json = match serde_json::to_string(&probe) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise provider probe payload: {error}");
            return 1;
        }
    };
    let payload_pretty = match serde_json::to_string_pretty(&probe) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to render provider probe payload: {error}");
            return 1;
        }
    };
    let _guard = DISPATCH_PROBE_LOCK.lock().await;
    let _payload_guard = ScopedEnvVar::set(PROBE_PAYLOAD_ENV, &payload_json);
    let _pretty_guard = ScopedEnvVar::set(PROBE_PAYLOAD_PRETTY_ENV, &payload_pretty);
    let outcome = dispatch::run_embedded_script("providers/probe", Vec::new(), args.json).await;
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    // The script's own exit code reflects readiness (1 on failure).
    // If the script itself errored at the 70 level (internal), surface
    // that to the user instead.
    if outcome.exit_code != 0 {
        outcome.exit_code
    } else {
        exit_code
    }
}

async fn aggregate_provider_probe(args: &ProviderProbeArgs) -> ProviderProbe {
    let readiness = probe_provider_readiness(
        &args.provider,
        args.model.as_deref(),
        args.base_url.as_deref(),
    )
    .await;

    let base_url = readiness.base_url.clone().or_else(|| {
        llm_config::provider_config(&args.provider).map(|def| llm_config::resolve_base_url(&def))
    });

    let loaded_models = if args.provider == "ollama" {
        let base = base_url
            .clone()
            .unwrap_or_else(|| "http://localhost:11434".to_string());
        match fetch_ollama_ps(&base).await {
            Ok(entries) => entries,
            Err(error) => {
                // `/api/ps` is best-effort: a daemon that doesn't expose it
                // shouldn't block the readiness signal. Warn so eval logs
                // surface the gap without failing the probe.
                eprintln!("warning: /api/ps unavailable: {error}");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    ProviderProbe {
        provider: args.provider.clone(),
        base_url,
        readiness,
        runtime_profile: if LOCAL_PROVIDERS.contains(&args.provider.as_str()) {
            args.model.as_deref().map(|model| {
                harn_vm::llm::local_profiles::local_runtime_profile_report(
                    model,
                    Some(&args.provider),
                )
            })
        } else {
            None
        },
        loaded_models,
    }
}

pub(crate) async fn run_provider_tool_probe(args: ProviderToolProbeArgs) {
    let exit_code = dispatch_provider_tool_probe(args).await;
    if exit_code != 0 {
        process::exit(exit_code);
    }
}

async fn dispatch_provider_tool_probe(args: ProviderToolProbeArgs) -> i32 {
    let report = match aggregate_tool_conformance_report(&args).await {
        Ok(report) => report,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    let payload_json = match serde_json::to_string(&report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise tool-probe payload: {error}");
            return 1;
        }
    };
    let payload_pretty = match serde_json::to_string_pretty(&report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to render tool-probe payload: {error}");
            return 1;
        }
    };
    let fallback_disabled = report.tool_calling.fallback_mode
        == harn_vm::llm::tool_conformance::ToolProbeFallbackMode::Disabled;
    let _guard = DISPATCH_TOOL_PROBE_LOCK.lock().await;
    let _payload_guard = ScopedEnvVar::set(TOOL_PROBE_PAYLOAD_ENV, &payload_json);
    let _pretty_guard = ScopedEnvVar::set(TOOL_PROBE_PAYLOAD_PRETTY_ENV, &payload_pretty);
    let outcome =
        dispatch::run_embedded_script("providers/tool_probe", Vec::new(), args.json).await;
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    if outcome.exit_code != 0 {
        return outcome.exit_code;
    }
    i32::from(fallback_disabled)
}

async fn aggregate_tool_conformance_report(
    args: &ProviderToolProbeArgs,
) -> Result<harn_vm::llm::tool_conformance::ToolConformanceReport, String> {
    if let Some(path) = args.response_fixture.as_ref() {
        if args.repeat != 1 {
            return Err(
                "error: --repeat is only supported for live provider tool probes; \
                 --response-fixture classifies one saved response"
                    .to_string(),
            );
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|error| format!("error: failed to read {}: {error}", path.display()))?;
        Ok(
            harn_vm::llm::tool_conformance::classify_tool_conformance_fixture(
                args.provider.clone(),
                args.model.clone(),
                modes_for_arg(args.mode)
                    .into_iter()
                    .next()
                    .unwrap_or(harn_vm::llm::tool_conformance::ToolProbeMode::NonStreaming),
                args.marker.clone(),
                &raw,
            ),
        )
    } else {
        let mut options = harn_vm::llm::tool_conformance::ToolConformanceProbeOptions::new(
            args.provider.clone(),
            args.model.clone(),
        );
        options.base_url = args.base_url.clone();
        options.modes = modes_for_arg(args.mode);
        options.marker = args.marker.clone();
        options.repeat = usize::from(args.repeat);
        options.timeout_secs = args.timeout_secs;
        Ok(harn_vm::llm::tool_conformance::run_tool_conformance_probe(options).await)
    }
}

pub(crate) async fn run_provider_cache_probe(args: ProviderCacheProbeArgs) {
    let exit_code = dispatch_provider_cache_probe(args).await;
    if exit_code != 0 {
        process::exit(exit_code);
    }
}

async fn dispatch_provider_cache_probe(args: ProviderCacheProbeArgs) -> i32 {
    let Some(fixture_path) = args.usage_fixture.as_ref() else {
        eprintln!(
            "error: live prompt-cache probing is not yet wired; pass --usage-fixture <path> \
             with a saved repeat-run usage fixture"
        );
        return 2;
    };
    let raw = match std::fs::read_to_string(fixture_path) {
        Ok(raw) => raw,
        Err(error) => {
            eprintln!("error: failed to read {}: {error}", fixture_path.display());
            return 1;
        }
    };
    let report = match harn_vm::llm::cache_conformance::classify_cache_conformance_fixture(
        args.provider.clone(),
        args.model.clone(),
        &raw,
    ) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    let dogfood_failure = report.dogfood_failure;
    let payload_json = match serde_json::to_string(&report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise cache-probe payload: {error}");
            return 1;
        }
    };
    let payload_pretty = match serde_json::to_string_pretty(&report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to render cache-probe payload: {error}");
            return 1;
        }
    };
    let _guard = DISPATCH_CACHE_PROBE_LOCK.lock().await;
    let _payload_guard = ScopedEnvVar::set(CACHE_PROBE_PAYLOAD_ENV, &payload_json);
    let _pretty_guard = ScopedEnvVar::set(CACHE_PROBE_PAYLOAD_PRETTY_ENV, &payload_pretty);
    let outcome =
        dispatch::run_embedded_script("providers/cache_probe", Vec::new(), args.json).await;
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    if outcome.exit_code != 0 {
        return outcome.exit_code;
    }
    // A supported route that never caches, or contradictory provider fields, is
    // a real conformance failure; a non-cache provider is not.
    i32::from(dogfood_failure)
}

fn modes_for_arg(
    mode: ProviderToolProbeModeArg,
) -> Vec<harn_vm::llm::tool_conformance::ToolProbeMode> {
    use harn_vm::llm::tool_conformance::ToolProbeMode;
    match mode {
        ProviderToolProbeModeArg::Both => {
            vec![ToolProbeMode::NonStreaming, ToolProbeMode::Streaming]
        }
        ProviderToolProbeModeArg::NonStreaming => vec![ToolProbeMode::NonStreaming],
        ProviderToolProbeModeArg::Streaming => vec![ToolProbeMode::Streaming],
    }
}

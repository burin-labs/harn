//! `harn provider probe` — one-shot machine-readable provider snapshot.
//!
//! Combines provider readiness (`/v1/models` or equivalent) with the
//! runtime-state details that local engines surface separately (Ollama's
//! `/api/ps` shows VRAM, size, expiry, and on newer builds the context
//! window the model was loaded with). Output is a structured envelope so
//! eval pipelines decode it with the same shape they use for per-call
//! `provider_telemetry`.
//!
//! ## .harn dispatch (W10 — see harn#2310)
//!
//! Aggregation stays in Rust (sandboxed scripts can't run the
//! readiness HTTP probe + `/api/ps` calls), the rendering layer lives
//! in `crates/harn-stdlib/src/stdlib/cli/providers/{probe,tool_probe}.harn`.
//! `HARN_CLI_IMPL=rust` keeps the legacy direct path for the parity-
//! snapshot harness (#2299) until the C1 ratchet (#2314) deletes it.

use std::io::Write as _;
use std::process;

use harn_vm::llm::readiness::{probe_provider_readiness, ProviderReadiness};
use harn_vm::llm_config;
use serde::Serialize;

use crate::cli::{ProviderProbeArgs, ProviderToolProbeArgs, ProviderToolProbeModeArg};
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

/// Serialises the dispatch path so concurrent in-process callers
/// don't race on the global env vars the shim sets.
static DISPATCH_PROBE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static DISPATCH_TOOL_PROBE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    if std::env::var("HARN_CLI_IMPL").as_deref() == Ok("rust") {
        run_provider_probe_legacy(args).await;
        return;
    }
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
            eprintln!("error: failed to serialise provider-probe payload: {error}");
            return 1;
        }
    };
    let payload_pretty = match serde_json::to_string_pretty(&probe) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to render provider-probe payload: {error}");
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
    // The script's own exit code reflects readiness (1 on failure),
    // matching the legacy path. If the script itself errored at the
    // 70 level (internal) we still want to surface that to the user.
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

/// Legacy direct-render path. Kept verbatim for the parity-snapshot
/// harness (#2299) until C1 (#2314) deletes it.
async fn run_provider_probe_legacy(args: ProviderProbeArgs) {
    let probe = aggregate_provider_probe(&args).await;
    let exit_code = i32::from(!probe.readiness.ok);

    if args.json {
        match serde_json::to_string_pretty(&probe) {
            Ok(payload) => println!("{payload}"),
            Err(error) => eprintln!("error: {error}"),
        }
    } else if probe.readiness.ok {
        println!("{}", probe.readiness.message);
        if !probe.loaded_models.is_empty() {
            println!("loaded:");
            for model in &probe.loaded_models {
                println!(
                    "  - {} (size={} vram={} ctx={} expires={})",
                    model.name,
                    fmt_bytes(model.size_bytes),
                    fmt_bytes(model.size_vram_bytes),
                    fmt_u64(model.context_length),
                    model.expires_at.as_deref().unwrap_or("-"),
                );
            }
        }
    } else {
        eprintln!("{}", probe.readiness.message);
    }

    if exit_code != 0 {
        process::exit(exit_code);
    }
}

pub(crate) async fn run_provider_tool_probe(args: ProviderToolProbeArgs) {
    if std::env::var("HARN_CLI_IMPL").as_deref() == Ok("rust") {
        run_provider_tool_probe_legacy(args).await;
        return;
    }
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
        options.timeout_secs = args.timeout_secs;
        Ok(harn_vm::llm::tool_conformance::run_tool_conformance_probe(options).await)
    }
}

/// Legacy direct-render path. Kept verbatim for the parity-snapshot
/// harness (#2299) until C1 (#2314) deletes it.
async fn run_provider_tool_probe_legacy(args: ProviderToolProbeArgs) {
    let report = match aggregate_tool_conformance_report(&args).await {
        Ok(report) => report,
        Err(error) => {
            eprintln!("{error}");
            process::exit(1);
        }
    };

    if args.json {
        match serde_json::to_string_pretty(&report) {
            Ok(payload) => println!("{payload}"),
            Err(error) => {
                eprintln!("error: failed to render probe JSON: {error}");
                process::exit(1);
            }
        }
    } else {
        println!(
            "{} {} fallback={} native={} text={} streaming_native={}",
            report.provider,
            report.model,
            report.tool_calling.fallback_mode.as_str(),
            report.tool_calling.native.as_str(),
            report.tool_calling.text.as_str(),
            report.tool_calling.streaming_native.as_str(),
        );
        for case in &report.cases {
            println!(
                "  {}: {:?} ok={} reason={}",
                case.mode.as_str(),
                case.classification,
                case.ok,
                case.failure_reason.as_deref().unwrap_or("-"),
            );
        }
    }

    if report.tool_calling.fallback_mode
        == harn_vm::llm::tool_conformance::ToolProbeFallbackMode::Disabled
    {
        process::exit(1);
    }
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

fn fmt_bytes(value: Option<u64>) -> String {
    value
        .map(|n| format!("{n}B"))
        .unwrap_or_else(|| "-".to_string())
}

fn fmt_u64(value: Option<u64>) -> String {
    value
        .map(|n| n.to_string())
        .unwrap_or_else(|| "-".to_string())
}

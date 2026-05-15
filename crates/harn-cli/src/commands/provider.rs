//! `harn provider probe` — one-shot machine-readable provider snapshot.
//!
//! Combines provider readiness (`/v1/models` or equivalent) with the
//! runtime-state details that local engines surface separately (Ollama's
//! `/api/ps` shows VRAM, size, expiry, and on newer builds the context
//! window the model was loaded with). Output is a structured envelope so
//! eval pipelines decode it with the same shape they use for per-call
//! `provider_telemetry`.

use std::process;

use harn_vm::llm::readiness::{probe_provider_readiness, ProviderReadiness};
use harn_vm::llm_config;
use serde::Serialize;

use crate::cli::{ProviderProbeArgs, ProviderToolProbeArgs, ProviderToolProbeModeArg};
use crate::commands::local::runtime::{fetch_ollama_ps, LoadedModel, LOCAL_PROVIDERS};

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

    let exit_code = if readiness.ok { 0 } else { 1 };

    let probe = ProviderProbe {
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
    };

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
    let report = if let Some(path) = args.response_fixture.as_ref() {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) => {
                eprintln!("error: failed to read {}: {error}", path.display());
                process::exit(1);
            }
        };
        harn_vm::llm::tool_conformance::classify_tool_conformance_fixture(
            args.provider.clone(),
            args.model.clone(),
            modes_for_arg(args.mode)
                .into_iter()
                .next()
                .unwrap_or(harn_vm::llm::tool_conformance::ToolProbeMode::NonStreaming),
            args.marker.clone(),
            &raw,
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
        harn_vm::llm::tool_conformance::run_tool_conformance_probe(options).await
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

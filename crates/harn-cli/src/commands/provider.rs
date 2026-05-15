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

use crate::cli::ProviderProbeArgs;
use crate::commands::local::runtime::{fetch_ollama_ps, LoadedModel};

#[derive(Debug, Serialize)]
struct ProviderProbe {
    provider: String,
    base_url: Option<String>,
    readiness: ProviderReadiness,
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

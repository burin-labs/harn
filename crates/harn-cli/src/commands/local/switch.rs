//! `harn local switch <alias>` — make a model the active local runtime.
//!
//! Steps, in order:
//! 1. Resolve the alias → `(provider, model_id)` through the provider catalog
//!    (with an explicit `--provider` override taking precedence).
//! 2. Pick `--ctx` / `--keep-alive` defaults from the machine profile if the
//!    user didn't pass them.
//! 3. Unless `--no-evict`, unload any other models held by sibling local
//!    providers (Ollama: drain `/api/ps`; llama.cpp/MLX: stop tracked PIDs).
//! 4. Provider-specific warm step:
//!    - Ollama: optional `ollama pull` (skipped with `--no-pull`),
//!      `/api/tags` probe, then a `/api/generate` warmup whose body
//!      sets `num_ctx` and `keep_alive` from this command's flags so
//!      the resident model honors the chosen window/lifetime.
//!    - llama.cpp / MLX / local / vLLM: probe `/v1/models`, then verify the
//!      requested model id is served. Re-probe once after a short delay to
//!      confirm the server didn't tear it down between calls.
//! 5. Persist the selection to `<state>/local/selection.json`.

use std::path::Path;
use std::time::Duration;

use harn_vm::llm::local_profiles::{
    evaluate_runtime_profile_gate, local_runtime_profile_report_for, LocalRuntimeProfileReport,
    RuntimeProbeEvidence, RuntimeProfileGate,
};
use harn_vm::llm::readiness::probe_provider_readiness;
use harn_vm::llm::{
    normalize_ollama_keep_alive, ollama_readiness, warm_ollama_model_with_settings,
    OllamaReadinessOptions, OllamaRuntimeSettings,
};
use harn_vm::llm_config::{self};
use serde::Serialize;

use crate::cli::LocalSwitchArgs;
use crate::commands::hardware::collect_hardware_snapshot;

use super::profile::defaults_for;
use super::runtime::{
    local_provider_ids, ollama_unload_model, resolve_provider_def, snapshot_provider, terminate_pid,
};
use super::state::{clear_pid_record, read_pid_record, write_selection, LocalSelection};

#[derive(Debug, Serialize)]
struct SwitchResult {
    provider: String,
    model: String,
    alias: Option<String>,
    base_url: String,
    ctx: u64,
    keep_alive: String,
    evicted: Vec<EvictionRecord>,
    readiness: serde_json::Value,
    rechecked: bool,
    runtime_profile: LocalRuntimeProfileReport,
    profile_gate: RuntimeProfileGate,
}

#[derive(Debug, Serialize)]
struct EvictionRecord {
    provider: String,
    target: String,
    outcome: String,
}

pub(crate) async fn run(args: LocalSwitchArgs, base_dir: &Path) -> Result<(), String> {
    let resolved = llm_config::resolve_model_info(&args.model);
    let provider = args
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| resolved.provider.clone());
    if !local_provider_ids(None).contains(&provider) {
        return Err(format!(
            "'{provider}' is not a local provider Harn manages (expected one of: {})",
            local_provider_ids(None).join(", ")
        ));
    }
    let runtime_profile =
        local_runtime_profile_report_for(resolved.alias.as_deref(), &resolved.id, &provider);
    let evidence = load_runtime_probe_evidence(&args)?;
    let profile_gate = evaluate_runtime_profile_gate(&runtime_profile, &evidence, args.force);
    if !profile_gate.allowed {
        return Err(format!(
            "{}. Run `harn local profile {} --provider {}` for details, pass \
             `--probe-result <provider-tool-probe.json>` after probing, or use `--force`.",
            profile_gate.message, args.model, provider
        ));
    }

    let def = resolve_provider_def(&provider)?;
    let base_url = llm_config::resolve_base_url(&def);
    let hardware = collect_hardware_snapshot();
    let defaults = defaults_for(&hardware);
    let ctx = args.ctx.unwrap_or(defaults.ctx);
    let keep_alive = args
        .keep_alive
        .clone()
        .unwrap_or_else(|| defaults.keep_alive.to_string());

    let evicted = if args.no_evict {
        Vec::new()
    } else {
        evict_siblings(&provider, &resolved.id, base_dir).await
    };

    let (readiness, rechecked) = match provider.as_str() {
        "ollama" => warm_ollama(&resolved.id, &base_url, ctx, &keep_alive, args.no_pull).await,
        _ => warm_openai_compatible(&provider, &resolved.id, &base_url).await,
    }?;

    let selection = LocalSelection::now(
        provider.clone(),
        resolved.id.clone(),
        resolved.alias.clone(),
        base_url.clone(),
        Some(ctx),
        Some(keep_alive.clone()),
    );
    write_selection(base_dir, &selection)?;

    let result = SwitchResult {
        provider,
        model: resolved.id,
        alias: resolved.alias,
        base_url,
        ctx,
        keep_alive,
        evicted,
        readiness,
        rechecked,
        runtime_profile,
        profile_gate,
    };

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result)
                .map_err(|error| format!("failed to render switch JSON: {error}"))?
        );
    } else {
        println!(
            "Activated {} via {} at {}",
            result.model, result.provider, result.base_url
        );
        println!(
            "  ctx={} keep_alive={} (machine profile)",
            result.ctx, result.keep_alive
        );
        for record in &result.evicted {
            println!(
                "  evicted {}::{} -> {}",
                record.provider, record.target, record.outcome
            );
        }
        if result.rechecked {
            println!("  readiness re-checked after warm");
        }
        if result.runtime_profile.requires_probe_gate {
            println!("  runtime profile: {}", result.profile_gate.message);
        }
    }
    Ok(())
}

fn load_runtime_probe_evidence(args: &LocalSwitchArgs) -> Result<RuntimeProbeEvidence, String> {
    let mut evidence = RuntimeProbeEvidence::new();
    for probe in &args.passed_probes {
        evidence.add_passed(probe.clone());
    }
    for path in &args.probe_results {
        let raw = std::fs::read_to_string(path)
            .map_err(|error| format!("failed to read probe result {}: {error}", path.display()))?;
        let report: harn_vm::llm::tool_conformance::ToolConformanceReport =
            serde_json::from_str(&raw).map_err(|error| {
                format!(
                    "failed to parse provider-tool-probe result {}: {error}",
                    path.display()
                )
            })?;
        evidence.add_tool_report(report);
    }
    Ok(evidence)
}

async fn evict_siblings(
    active_provider: &str,
    active_model: &str,
    base_dir: &Path,
) -> Vec<EvictionRecord> {
    let mut evicted = Vec::new();
    for provider in local_provider_ids(None) {
        let Ok(snapshot) = snapshot_provider(&provider, base_dir).await else {
            continue;
        };
        let keep_model = (provider == active_provider).then_some(active_model);
        if provider == "ollama" {
            drain_ollama(&snapshot, keep_model, &mut evicted).await;
        }
        if provider == active_provider {
            continue;
        }
        if let Ok(Some(record)) = read_pid_record(base_dir, &provider) {
            // llama.cpp / MLX / local / vLLM: if Harn launched the
            // process, shut it down so the active runtime can take the
            // VRAM/RAM.
            let outcome = match terminate_pid(record.pid) {
                Ok(()) => "stopped".to_string(),
                Err(error) => format!("error: {error}"),
            };
            let _ = clear_pid_record(base_dir, &provider);
            evicted.push(EvictionRecord {
                provider: provider.clone(),
                target: format!("pid {}", record.pid),
                outcome,
            });
        }
    }
    evicted
}

/// Unload everything Ollama has resident, optionally keeping a single
/// model that matches `keep` (the model we're about to warm). Pulling the
/// same model we're keeping would just rewrite its keep-alive; pulling
/// anything else costs RAM/VRAM we want the active runtime to use.
async fn drain_ollama(
    snapshot: &super::runtime::LocalProviderSnapshot,
    keep: Option<&str>,
    evicted: &mut Vec<EvictionRecord>,
) {
    for loaded in &snapshot.loaded_models {
        if keep.is_some_and(|name| ollama_name_matches(&loaded.name, name)) {
            continue;
        }
        let outcome = match ollama_unload_model(&snapshot.base_url, &loaded.name).await {
            Ok(()) => "unloaded".to_string(),
            Err(error) => format!("error: {error}"),
        };
        evicted.push(EvictionRecord {
            provider: snapshot.provider.clone(),
            target: loaded.name.clone(),
            outcome,
        });
    }
}

fn ollama_name_matches(loaded_name: &str, requested: &str) -> bool {
    loaded_name == requested
        || loaded_name.strip_suffix(":latest") == Some(requested)
        || loaded_name.starts_with(requested)
}

async fn warm_ollama(
    model: &str,
    base_url: &str,
    ctx: u64,
    keep_alive: &str,
    no_pull: bool,
) -> Result<(serde_json::Value, bool), String> {
    if !no_pull {
        // Best-effort pull when the local daemon doesn't have the model.
        // Failure here is non-fatal — the readiness probe below will give
        // a clear `model_missing` message in that case.
        if let Err(error) = ensure_ollama_model_pulled(model).await {
            eprintln!("warning: ollama pull skipped: {error}");
        }
    }

    // 1. Confirm daemon + model present before we hand any work to the
    //    warmup endpoint. Empty-prompt warmup against a missing model would
    //    return a less useful error.
    let mut probe = OllamaReadinessOptions::new(model);
    probe.base_url = Some(base_url.to_string());
    probe.warm = false;
    let first = ollama_readiness(probe.clone()).await;
    if !first.valid {
        return Ok((
            serde_json::to_value(&first).map_err(|error| error.to_string())?,
            false,
        ));
    }

    // 2. Warm with the explicit ctx/keep-alive so the loaded model honors
    //    the machine profile (or user override). `OllamaRuntimeSettings`
    //    embeds num_ctx into the /api/generate body so Ollama allocates
    //    the right KV cache at load time, not at the first chat request.
    let settings = OllamaRuntimeSettings {
        num_ctx: ctx,
        keep_alive: normalize_ollama_keep_alive(keep_alive)
            .unwrap_or_else(|| serde_json::json!(keep_alive)),
    };
    if let Err(error) = warm_ollama_model_with_settings(model, Some(base_url), &settings).await {
        return Ok((
            serde_json::json!({
                "valid": false,
                "status": "warmup_failed",
                "message": error,
            }),
            false,
        ));
    }

    // 3. Re-probe: the issue calls out "verify the model remains reachable
    //    after initial /v1/models success." For Ollama a second /api/tags
    //    round-trip catches the case where the warmup booted the model out.
    let second = ollama_readiness(probe).await;
    Ok((
        serde_json::to_value(&second).map_err(|error| error.to_string())?,
        true,
    ))
}

async fn warm_openai_compatible(
    provider: &str,
    model: &str,
    base_url: &str,
) -> Result<(serde_json::Value, bool), String> {
    let first = probe_provider_readiness(provider, Some(model), Some(base_url)).await;
    if !first.ok {
        return Ok((
            serde_json::to_value(&first).map_err(|error| error.to_string())?,
            false,
        ));
    }
    // The issue calls out re-probing after the first success: some local
    // servers respond to /v1/models before the model weights are fully
    // mapped. Wait a beat, then re-probe to confirm the model is still
    // served. The second result is what we return so any post-warm
    // degradation surfaces in the caller's exit code.
    tokio::time::sleep(Duration::from_millis(250)).await;
    let second = probe_provider_readiness(provider, Some(model), Some(base_url)).await;
    Ok((
        serde_json::to_value(&second).map_err(|error| error.to_string())?,
        true,
    ))
}

async fn ensure_ollama_model_pulled(model: &str) -> Result<(), String> {
    if which::which("ollama").is_err() {
        return Err("ollama CLI not on PATH".to_string());
    }
    let status = tokio::process::Command::new("ollama")
        .arg("pull")
        .arg(model)
        .status()
        .await
        .map_err(|error| format!("failed to spawn ollama pull: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("ollama pull exited {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::ollama_name_matches;

    #[test]
    fn ollama_name_matches_accepts_exact_and_latest_suffix() {
        assert!(ollama_name_matches("qwen3:30b", "qwen3:30b"));
        assert!(ollama_name_matches("qwen3:30b:latest", "qwen3:30b"));
        assert!(ollama_name_matches("qwen3:30b-a3b-instruct", "qwen3:30b"));
        assert!(!ollama_name_matches("llama3.2", "qwen3:30b"));
    }
}

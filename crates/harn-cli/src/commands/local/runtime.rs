//! Shared helpers for `harn local …` subcommands: provider enumeration,
//! readiness snapshots, Ollama `/api/ps` loaded-model details, and PID-file
//! tracking for Harn-launched processes.

use std::path::Path;
use std::time::Duration;

use harn_vm::llm::api::OllamaPsModel;
use harn_vm::llm::readiness::{probe_provider_readiness, ProviderReadiness, ReadinessStatus};
use harn_vm::llm_config::{self, ProviderDef};
use serde::Serialize;

use crate::net;

use super::state::{read_pid_record, PidRecord};

pub(crate) fn normalize_local_provider_id(provider: &str) -> String {
    let trimmed = provider.trim();
    let normalized = trimmed.to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "ollama" | "local-ollama" => "ollama".to_string(),
        "llamacpp" | "llama.cpp" | "llama-cpp" | "local-llamacpp" | "local-llama.cpp"
        | "local-llama-cpp" => "llamacpp".to_string(),
        "mlx" | "local-mlx" => "mlx".to_string(),
        "vllm" | "local-vllm" | "vllm-local" => "vllm".to_string(),
        _ => trimmed.to_string(),
    }
}

/// Per-provider runtime snapshot. Combines:
/// - provider catalog metadata (base_url, port, env override, auth style),
/// - liveness via `/v1/models` (or `/api/tags` for Ollama),
/// - currently-loaded models with memory footprint (Ollama `/api/ps` only),
/// - any harn-launched PID we are tracking.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LocalProviderSnapshot {
    pub provider: String,
    pub display_name: Option<String>,
    pub base_url: String,
    pub base_url_env: Option<String>,
    pub port: Option<u16>,
    pub reachable: bool,
    pub readiness_status: String,
    pub message: String,
    pub served_models: Vec<String>,
    pub loaded_models: Vec<LoadedModel>,
    pub pid_record: Option<PidRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LoadedModel {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_vram_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Context window the model was loaded with. Surfaced where Ollama
    /// includes it in `/api/ps` (newer daemons; older builds return None).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
}

pub(crate) fn local_provider_ids(filter: Option<&str>) -> Vec<String> {
    if let Some(name) = filter.map(str::trim).filter(|name| !name.is_empty()) {
        let id = normalize_local_provider_id(name);
        return local_runtime_for_provider(&id)
            .is_some()
            .then_some(id)
            .into_iter()
            .collect();
    }
    llm_config::provider_names()
        .into_iter()
        .filter(|id| local_runtime_for_provider(id).is_some())
        .collect()
}

/// Return lifecycle metadata from the effective provider catalog. Local
/// commands use this rather than a parallel provider-name list so catalog
/// additions are listable, launchable when managed, and never mis-owned.
pub(crate) fn local_runtime_for_provider(
    provider: &str,
) -> Option<harn_vm::llm_config::LocalRuntimeDef> {
    let provider = normalize_local_provider_id(provider);
    llm_config::provider_config(&provider).and_then(|def| def.local_runtime)
}

pub(crate) async fn snapshot_provider(
    provider: &str,
    state_dir: &Path,
) -> Result<LocalProviderSnapshot, String> {
    let provider = normalize_local_provider_id(provider);
    let def = llm_config::provider_config(&provider)
        .ok_or_else(|| format!("unknown provider: {provider}"))?;
    let base_url = llm_config::resolve_base_url(&def);

    let (reachable, status, message, served_models) = if provider == "ollama" {
        snapshot_ollama_reachability(&base_url).await
    } else {
        snapshot_openai_reachability(&provider, &base_url).await
    };

    let loaded_models = if provider == "ollama" && reachable {
        fetch_ollama_ps(&base_url).await.unwrap_or_default()
    } else {
        Vec::new()
    };

    Ok(LocalProviderSnapshot {
        provider: provider.clone(),
        display_name: def.display_name.clone(),
        base_url: base_url.clone(),
        base_url_env: def.base_url_env.clone(),
        port: port_from_base_url(&base_url),
        reachable,
        readiness_status: status,
        message,
        served_models,
        loaded_models,
        pid_record: read_pid_record(state_dir, &provider).ok().flatten(),
    })
}

async fn snapshot_openai_reachability(
    provider: &str,
    base_url: &str,
) -> (bool, String, String, Vec<String>) {
    let readiness = probe_provider_readiness(provider, None, Some(base_url)).await;
    let ProviderReadiness {
        ok,
        status,
        message,
        served_models,
        ..
    } = readiness;
    (ok, readiness_status_label(status), message, served_models)
}

async fn snapshot_ollama_reachability(base_url: &str) -> (bool, String, String, Vec<String>) {
    // `ollama_readiness` is keyed on a specific model id and returns
    // misleading messages when we just want a liveness check. Hit
    // `/api/tags` directly and collect the served set.
    match fetch_ollama_tags(base_url).await {
        Ok(served) => {
            let message = format!("Ollama is reachable at {base_url}; {} served", served.len());
            (true, "ok".to_string(), message, served)
        }
        Err(OllamaProbeError { status, message }) => (false, status, message, Vec::new()),
    }
}

#[derive(Debug)]
struct OllamaProbeError {
    status: String,
    message: String,
}

async fn fetch_ollama_tags(base_url: &str) -> Result<Vec<String>, OllamaProbeError> {
    let url = ollama_endpoint(base_url, "/api/tags").map_err(|message| OllamaProbeError {
        status: "invalid_url".to_string(),
        message,
    })?;
    let client = local_http_client().map_err(|message| OllamaProbeError {
        status: "client_error".to_string(),
        message,
    })?;
    let response = client
        .get(url.clone())
        .header("Content-Type", "application/json")
        .timeout(Duration::from_secs(4))
        .send()
        .await
        .map_err(|error| OllamaProbeError {
            status: "unreachable".to_string(),
            message: format!("Ollama daemon not reachable at {url}: {error}"),
        })?;
    if !response.status().is_success() {
        return Err(OllamaProbeError {
            status: "bad_status".to_string(),
            message: format!("Ollama /api/tags returned HTTP {}", response.status()),
        });
    }
    let body: serde_json::Value = response.json().await.map_err(|error| OllamaProbeError {
        status: "bad_response".to_string(),
        message: format!("Ollama /api/tags returned unparsable body: {error}"),
    })?;
    Ok(body
        .get("models")
        .and_then(|value| value.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    entry
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default())
}

/// Build an Ollama endpoint URL, normalizing `localhost` → `127.0.0.1` (some
/// Ollama builds reject the literal hostname). One canonical helper so
/// every call site is consistent.
fn ollama_endpoint(base_url: &str, path: &str) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(base_url)
        .map_err(|error| format!("invalid Ollama base URL '{base_url}': {error}"))?;
    if url.host_str() == Some("localhost") {
        url.set_host(Some("127.0.0.1"))
            .map_err(|_| format!("invalid Ollama base URL '{base_url}'"))?;
    }
    url.set_path(path);
    Ok(url)
}

fn readiness_status_label(status: ReadinessStatus) -> String {
    match status {
        ReadinessStatus::Ok => "ok",
        ReadinessStatus::UnknownProvider => "unknown_provider",
        ReadinessStatus::Unsupported => "unsupported",
        ReadinessStatus::InvalidUrl => "invalid_url",
        ReadinessStatus::Unreachable => "unreachable",
        ReadinessStatus::BadStatus => "bad_status",
        ReadinessStatus::BadResponse => "bad_response",
        ReadinessStatus::ModelMissing => "model_missing",
        ReadinessStatus::ProviderMismatch => "provider_mismatch",
    }
    .to_string()
}

pub(crate) fn port_from_base_url(base_url: &str) -> Option<u16> {
    reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.port())
}

/// Best-effort `/api/ps` query so `harn local list` can show which Ollama
/// models are loaded right now plus their memory footprint. We swallow
/// errors so list/status keep working when the daemon is older or the
/// endpoint is unavailable.
pub(crate) async fn fetch_ollama_ps(base_url: &str) -> Result<Vec<LoadedModel>, String> {
    let url = ollama_endpoint(base_url, "/api/ps")?;
    let client = local_http_client()?;
    let response = client
        .get(url)
        .header("Content-Type", "application/json")
        .timeout(Duration::from_secs(4))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("/api/ps returned HTTP {}", response.status()));
    }
    let body: serde_json::Value = response.json().await.map_err(|error| error.to_string())?;
    let models = body.get("models").and_then(|value| value.as_array());
    let Some(models) = models else {
        return Ok(Vec::new());
    };
    // The harn-vm `OllamaPsModel` parser is the canonical shape, so per-call
    // telemetry and per-snapshot loaded-model listings can't drift: any new
    // upstream field (e.g. context window changes) lands in one place.
    Ok(models
        .iter()
        .filter_map(|entry| {
            let ps = OllamaPsModel::from_ps_entry(entry)?;
            let name = ps.name?;
            Some(LoadedModel {
                name,
                size_bytes: ps.size_bytes,
                size_vram_bytes: ps.size_vram_bytes,
                expires_at: ps.expires_at,
                context_length: ps.context_length,
            })
        })
        .collect())
}

/// Post `keep_alive=0` to `/api/generate` for `model`. Ollama treats that as
/// "unload after this request", which matches `ollama stop <model>` from the
/// CLI. Returns Ok on 2xx, Err with the upstream message otherwise.
pub(crate) async fn ollama_unload_model(base_url: &str, model: &str) -> Result<(), String> {
    let url = ollama_endpoint(base_url, "/api/generate")?;
    let body = serde_json::json!({
        "model": model,
        "prompt": "",
        "stream": false,
        "keep_alive": 0,
    });
    let client = local_http_client()?;
    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .timeout(Duration::from_secs(8))
        .json(&body)
        .send()
        .await
        .map_err(|error| format!("Ollama unload failed: {error}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        Err(format!(
            "Ollama unload returned HTTP {}: {}",
            status.as_u16(),
            detail
        ))
    }
}

/// SIGTERM a child PID Harn previously launched (llama.cpp / MLX). We
/// deliberately do not retry with SIGKILL: a stuck launcher is a signal
/// for the user to investigate, not for us to silently force-kill.
#[cfg(unix)]
pub(crate) fn terminate_pid(pid: u32) -> Result<(), String> {
    use std::convert::TryFrom;
    let raw = i32::try_from(pid).map_err(|_| format!("pid {pid} is out of range"))?;
    // SAFETY: `libc::kill` is FFI; we pass a well-formed PID + SIGTERM and
    // check the return code. No memory is shared with C.
    let rc = unsafe { libc::kill(raw, libc::SIGTERM) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

#[cfg(not(unix))]
pub(crate) fn terminate_pid(pid: u32) -> Result<(), String> {
    Err(format!(
        "terminating pid {pid} is not supported on this platform"
    ))
}

pub(crate) fn resolve_provider_def(provider: &str) -> Result<ProviderDef, String> {
    let provider = normalize_local_provider_id(provider);
    llm_config::provider_config(&provider)
        .ok_or_else(|| format!("unknown provider '{provider}' in Harn provider catalog"))
}

/// A fresh reqwest client for one-off lifecycle calls. We do not reuse
/// harn-vm's shared utility client because `harn local` is the only caller
/// from the CLI side and we want deterministic timeouts independent of the
/// streaming/transport client settings.
fn local_http_client() -> Result<reqwest::Client, String> {
    net::http_client_builder("cli.local.runtime")
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| {
            format!(
                "failed to build HTTP client: {}",
                net::reqwest_error(&error)
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_vm::llm_config::{LocalRuntimeKind, LocalRuntimeStop};

    #[test]
    fn local_provider_ids_filters_unknown_filter() {
        let ids = local_provider_ids(Some("ollama"));
        assert_eq!(ids, vec!["ollama".to_string()]);
        assert!(local_provider_ids(Some("openai")).is_empty());
    }

    #[test]
    fn local_provider_ids_canonicalizes_local_runtime_aliases() {
        assert_eq!(
            local_provider_ids(Some("local-vllm")),
            vec!["vllm".to_string()]
        );
        assert_eq!(
            local_provider_ids(Some("local-llama.cpp")),
            vec!["llamacpp".to_string()]
        );
        assert_eq!(
            local_provider_ids(Some("local_ollama")),
            vec!["ollama".to_string()]
        );
        assert!(local_provider_ids(Some("not-a-provider")).is_empty());
    }

    #[test]
    fn local_provider_ids_derive_sorted_catalog_lifecycle_rows() {
        let ids = local_provider_ids(None);
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(ids.contains(&"ollama".to_string()));
        assert!(ids.contains(&"llamacpp".to_string()));
        assert!(ids.contains(&"mlx".to_string()));
        assert!(ids.contains(&"local".to_string()));
        assert!(ids.contains(&"tgi".to_string()));
        assert!(ids.contains(&"vllm".to_string()));
        for id in ids {
            assert!(
                local_runtime_for_provider(&id).is_some(),
                "{id} must be catalog-declared"
            );
        }
    }

    #[test]
    fn generic_local_runtime_is_explicitly_external() {
        let runtime = local_runtime_for_provider("local")
            .expect("generic local provider has catalog lifecycle metadata");
        assert_eq!(runtime.kind, Some(LocalRuntimeKind::External));
        assert_eq!(runtime.stop, Some(LocalRuntimeStop::External));
    }

    #[test]
    fn port_from_base_url_recognizes_explicit_port() {
        assert_eq!(port_from_base_url("http://127.0.0.1:11434"), Some(11434));
        assert_eq!(port_from_base_url("http://localhost:8001/v1"), Some(8001));
        assert_eq!(port_from_base_url("https://api.example.com"), None);
    }

    #[test]
    fn readiness_status_label_projects_provider_mismatch() {
        assert_eq!(
            readiness_status_label(ReadinessStatus::ProviderMismatch),
            "provider_mismatch"
        );
    }
}

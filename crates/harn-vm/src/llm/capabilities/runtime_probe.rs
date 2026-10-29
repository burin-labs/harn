//! Endpoint-owned runtime capability measurement for self-hosted providers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::OnceCell;

use super::model::{Capabilities, CapabilityProbeReceipt, CapabilityProbeStatus};

type ProbeCell = Arc<OnceCell<CapabilityProbeReceipt>>;

static PROBES: OnceLock<Mutex<HashMap<String, ProbeCell>>> = OnceLock::new();

#[derive(Clone, Copy)]
enum ProbeKind {
    LlamaProps,
    OllamaShow,
}

impl ProbeKind {
    fn for_provider(provider: &str) -> Option<Self> {
        match provider.trim().to_ascii_lowercase().as_str() {
            "local" | "llamacpp" | "mlx" => Some(Self::LlamaProps),
            "ollama" => Some(Self::OllamaShow),
            _ => None,
        }
    }
}

/// Measure the loaded self-hosted runtime once. Unsupported probe endpoints
/// are a cached, explicit result; they do not prevent the real model call.
/// Returns true when this call installed a result that later capability
/// lookups must observe. A cached result was already visible to the caller.
/// A branch prefix refuses a probe that would reach the network: the tape
/// does not hold its answer.
pub(crate) async fn ensure_runtime_probe(
    provider: &str,
    model: &str,
) -> Result<bool, crate::value::VmError> {
    let Some(kind) = ProbeKind::for_provider(provider) else {
        return Ok(false);
    };
    let resolved = crate::llm::helpers::ResolvedProvider::resolve(provider);
    if crate::run_tape::prefix_active()
        && cached_measurement_with(model, kind, &resolved.base_url).is_none()
    {
        crate::run_tape::refuse_in_prefix(
            "self-hosted capability probe",
            "it measures the runtime over the network",
        )?;
    }
    Ok(ensure_runtime_probe_with(provider, model, kind, resolved).await)
}

async fn ensure_runtime_probe_with(
    provider: &str,
    model: &str,
    kind: ProbeKind,
    resolved: crate::llm::helpers::ResolvedProvider,
) -> bool {
    let Some(request_endpoint) = probe_endpoint(&resolved.base_url, kind, false) else {
        return false;
    };
    let Some(public_endpoint) = probe_endpoint(&resolved.base_url, kind, true) else {
        return false;
    };
    let cache_key = cache_key(model, &public_endpoint, kind);
    let cell = {
        let mut probes = PROBES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .expect("runtime capability probe cache poisoned");
        probes
            .entry(cache_key)
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone()
    };
    let provider_label = provider.to_string();
    let model_label = model.to_string();
    let provider_for_probe = provider_label.clone();
    let model_for_probe = model_label.clone();
    let endpoint_for_probe = request_endpoint;
    let public_endpoint_for_probe = public_endpoint;
    let resolved_for_probe = resolved;
    let was_cached = cell.initialized();
    let receipt = cell
        .get_or_init(|| async move {
            probe(
                kind,
                &provider_for_probe,
                &model_for_probe,
                endpoint_for_probe,
                public_endpoint_for_probe,
                resolved_for_probe,
            )
            .await
        })
        .await;
    if !was_cached && receipt.status == CapabilityProbeStatus::Unavailable {
        tracing::warn!(
            target: "harn::llm::capabilities",
            provider = provider_label,
            model = model_label,
            detail = receipt.detail,
            "self-hosted capability probe unavailable; using the explicit catalog fallback"
        );
    }
    !was_cached
}

pub(super) fn apply_cached_measurement(provider: &str, model: &str, caps: &mut Capabilities) {
    if let Some(receipt) = cached_measurement(provider, model) {
        harn_provider_catalog::capabilities::apply_probe_receipt(caps, receipt);
    }
}

pub(crate) fn cached_measurement(provider: &str, model: &str) -> Option<CapabilityProbeReceipt> {
    let kind = ProbeKind::for_provider(provider)?;
    let resolved = crate::llm::helpers::ResolvedProvider::resolve_without_capabilities(provider);
    cached_measurement_with(model, kind, &resolved.base_url)
}

fn cached_measurement_with(
    model: &str,
    kind: ProbeKind,
    base_url: &str,
) -> Option<CapabilityProbeReceipt> {
    let endpoint = probe_endpoint(base_url, kind, true)?;
    let key = cache_key(model, &endpoint, kind);
    PROBES
        .get()
        .and_then(|probes| probes.lock().ok()?.get(&key).cloned())
        .and_then(|cell| cell.get().cloned())
}

async fn probe(
    kind: ProbeKind,
    provider: &str,
    model: &str,
    request_endpoint: String,
    public_endpoint: String,
    resolved: crate::llm::helpers::ResolvedProvider,
) -> CapabilityProbeReceipt {
    let api_key = crate::llm::resolve_api_key(provider).unwrap_or_default();
    let client = crate::llm::utility_client_for_base_url(&resolved.base_url);
    let request = match kind {
        ProbeKind::LlamaProps => client.get(&request_endpoint),
        ProbeKind::OllamaShow => client
            .post(&request_endpoint)
            .json(&serde_json::json!({"model": model})),
    };
    let response = resolved.apply_headers(request, &api_key).send().await;
    let value = match response {
        Ok(response) if response.status().is_success() => {
            match response.json::<serde_json::Value>().await {
                Ok(value) => value,
                Err(error) => {
                    return unavailable(
                        public_endpoint,
                        format!("response was not valid JSON: {error}"),
                    )
                }
            }
        }
        Ok(response) => {
            return unavailable(
                public_endpoint,
                format!("probe returned HTTP {}", response.status().as_u16()),
            )
        }
        Err(error) => {
            return unavailable(public_endpoint, format!("probe request failed: {error}"))
        }
    };
    let measurement = match kind {
        ProbeKind::LlamaProps => {
            let caps = value
                .get("chat_template_caps")
                .and_then(serde_json::Value::as_object);
            caps.map(|caps| {
                let accepts = caps
                    .get("supports_tools")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let emits = caps
                    .get("supports_tool_calls")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let parallel = caps
                    .get("supports_parallel_tool_calls")
                    .and_then(serde_json::Value::as_bool);
                (accepts && emits, parallel)
            })
        }
        ProbeKind::OllamaShow => value
            .get("capabilities")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                (
                    items.iter().any(|item| item.as_str() == Some("tools")),
                    None,
                )
            }),
    };
    let Some((native_tools, parallel)) = measurement else {
        return unavailable(
            public_endpoint,
            "probe response omitted tool capability data".to_string(),
        );
    };
    CapabilityProbeReceipt {
        status: CapabilityProbeStatus::Measured,
        endpoint: public_endpoint,
        native_tools: Some(native_tools),
        supports_parallel_tool_calls: parallel,
        detail: "the running server reported its loaded template capabilities".to_string(),
    }
}

fn unavailable(endpoint: String, detail: String) -> CapabilityProbeReceipt {
    CapabilityProbeReceipt {
        status: CapabilityProbeStatus::Unavailable,
        endpoint,
        native_tools: None,
        supports_parallel_tool_calls: None,
        detail,
    }
}

fn cache_key(model: &str, endpoint: &str, kind: ProbeKind) -> String {
    let model_key = if matches!(kind, ProbeKind::OllamaShow) {
        model
    } else {
        ""
    };
    format!("{endpoint}\n{model_key}")
}

fn probe_endpoint(base_url: &str, kind: ProbeKind, redact_credentials: bool) -> Option<String> {
    let mut url = reqwest::Url::parse(base_url).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let mut path = url.path().trim_end_matches('/').to_string();
    if path.ends_with("/v1") {
        path.truncate(path.len() - 3);
    }
    path.push_str(match kind {
        ProbeKind::LlamaProps => "/props",
        ProbeKind::OllamaShow => "/api/show",
    });
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    if redact_credentials {
        url.set_username("").ok()?;
        url.set_password(None).ok()?;
    }
    Some(url.to_string())
}

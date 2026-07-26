//! Provider reachability: credential validation, model readiness, and the
//! Ollama daemon warm-up probe.

use crate::llm::helpers::vm_value_to_json;
use crate::llm_config;
use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

/// Validate provider health, API key reachability, and optional model readiness.
#[harn_builtin(
    sig = "llm_healthcheck(provider_or_options?: string|dict, options?: dict|nil) -> dict",
    kind = "async",
    category = "llm.config"
)]
async fn llm_healthcheck_builtin(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let (provider_name, api_key) = parse_healthcheck_args(&args);

    // Ollama-specific readiness probe (issue #675): supports `model`,
    // `warm`, `base_url`, and `keep_alive` options to verify the daemon
    // and optionally pre-warm a tag before the first chat call.
    if provider_name == "ollama" {
        let options = args
            .iter()
            .filter_map(|value| value.as_dict())
            .find(|dict| {
                dict.contains_key("model")
                    || dict.contains_key("warm")
                    || dict.contains_key("preload")
                    || dict.contains_key("base_url")
                    || dict.contains_key("url")
                    || dict.contains_key("keep_alive")
            });
        let model = options
            .and_then(|opts| opts.get("model"))
            .map(|value| value.display())
            .or_else(|| crate::test_env::env_var_seamed("HARN_LLM_MODEL"))
            .or_else(|| crate::test_env::env_var_seamed("LOCAL_LLM_MODEL"));
        let warm = options
            .and_then(|opts| opts.get("warm").or_else(|| opts.get("preload")))
            .is_some_and(|value| matches!(value, VmValue::Bool(true)));

        if warm && model.as_deref().unwrap_or("").is_empty() {
            return Ok(json_to_vm_value(&serde_json::json!({
                "valid": false,
                "status": "invalid_request",
                "message": "Ollama warmup requires options.model",
            })));
        }

        if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
            let (resolved_model, _) = llm_config::resolve_model(&model);
            let mut readiness = crate::llm::api::OllamaReadinessOptions::new(resolved_model);
            readiness.warm = warm;
            readiness.observe_loaded = options
                .and_then(|opts| opts.get("observe_loaded"))
                .is_some_and(|value| matches!(value, VmValue::Bool(true)));
            readiness.base_url = options
                .and_then(|opts| opts.get("base_url").or_else(|| opts.get("url")))
                .map(|value| value.display())
                .filter(|value| !value.trim().is_empty());
            readiness.keep_alive = options
                .and_then(|opts| opts.get("keep_alive"))
                .map(|value| match value {
                    VmValue::String(raw) => crate::llm::api::normalize_ollama_keep_alive(raw)
                        .unwrap_or_else(|| vm_value_to_json(value)),
                    _ => vm_value_to_json(value),
                });
            let result = crate::llm::api::ollama_readiness(readiness).await;
            return Ok(json_to_vm_value(
                &serde_json::to_value(&result).unwrap_or_else(|_| {
                    serde_json::json!({
                        "valid": false,
                        "status": "serialization_error",
                        "message": "failed to serialize Ollama readiness result",
                    })
                }),
            ));
        }
    }

    let requested_model = healthcheck_model_arg(&args)
        .or_else(|| crate::llm::selected_model_for_provider(&provider_name));

    if let Some(model) = requested_model.filter(|model| !model.trim().is_empty()) {
        if let Some(pdef) = llm_config::provider_config(&provider_name) {
            if crate::llm::supports_model_readiness_probe(&pdef) {
                let key = api_key
                    .clone()
                    .or_else(|| crate::llm::resolve_api_key(&provider_name).ok())
                    .unwrap_or_default();
                let readiness = crate::llm::readiness::probe_provider_readiness_with_options(
                    &provider_name,
                    crate::llm::readiness::ProviderReadinessOptions {
                        requested_model: Some(&model),
                        base_url_override: None,
                        api_key_override: Some(&key),
                    },
                )
                .await;
                let json = serde_json::to_value(readiness).map_err(|error| {
                    VmError::Runtime(format!("llm_healthcheck: serialize readiness: {error}"))
                })?;
                return Ok(crate::schema::json_to_vm_value(&json));
            }
        }
    }

    let result = crate::llm::run_provider_healthcheck_with_options(
        &provider_name,
        crate::llm::ProviderHealthcheckOptions {
            api_key,
            client: None,
        },
    )
    .await;
    let json = serde_json::to_value(result)
        .map_err(|error| VmError::Runtime(format!("llm_healthcheck: serialize result: {error}")))?;
    Ok(crate::schema::json_to_vm_value(&json))
}

fn parse_healthcheck_args(args: &[VmValue]) -> (String, Option<String>) {
    let mut provider = "anthropic".to_string();
    let mut api_key = None;

    if let Some(first) = args.first() {
        if let Some(dict) = first.as_dict() {
            if let Some(value) = dict.get("provider") {
                provider = value.display();
            }
            if let Some(value) = dict.get("api_key") {
                api_key = Some(value.display());
            }
        } else {
            provider = first.display();
        }
    }

    if let Some(options) = args.get(1).and_then(|value| value.as_dict()) {
        if let Some(value) = options.get("api_key") {
            api_key = Some(value.display());
        }
    }

    (provider, api_key)
}

fn healthcheck_model_arg(args: &[VmValue]) -> Option<String> {
    let dict_model = args
        .first()
        .and_then(|value| value.as_dict())
        .and_then(|dict| dict.get("model").or_else(|| dict.get("alias")))
        .map(|value| value.display());
    let raw = match dict_model {
        Some(value) => value,
        None => match args.get(1) {
            Some(VmValue::Dict(dict)) => dict
                .get("model")
                .or_else(|| dict.get("alias"))
                .map(|value| value.display())?,
            Some(VmValue::Nil) => return None,
            Some(value) => value.display(),
            None => return None,
        },
    };
    let (resolved, _) = llm_config::resolve_model(raw.trim());
    Some(resolved)
}

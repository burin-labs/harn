use std::collections::BTreeMap;
use std::rc::Rc;

use crate::llm_config;
use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::{harn_builtin, register_builtin_defs, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::helpers::vm_value_to_json;

/// Register config-based LLM builtins (llm_infer_provider, llm_resolve_model, etc.).
pub(crate) fn register_config_builtins(vm: &mut Vm) {
    register_builtin_defs(vm, LLM_CONFIG_DEFS);
}

const LLM_CONFIG_DEFS: &[&VmBuiltinDef] = &[
    &PROVIDER_CAPABILITIES_BUILTIN_DEF,
    &PROVIDER_CAPABILITIES_INSTALL_BUILTIN_DEF,
    &PROVIDER_CAPABILITIES_CLEAR_BUILTIN_DEF,
    &LLM_INFER_PROVIDER_BUILTIN_DEF,
    &LLM_MODEL_TIER_BUILTIN_DEF,
    &LLM_RESOLVE_MODEL_BUILTIN_DEF,
    &LLM_MODEL_INFO_BUILTIN_DEF,
    &LLM_KNOWN_MODELS_BUILTIN_DEF,
    &LLM_AVAILABLE_PROVIDERS_BUILTIN_DEF,
    &LLM_QC_DEFAULT_MODEL_BUILTIN_DEF,
    &LLM_RESOLVED_OPTIONS_BUILTIN_DEF,
    &LLM_APPLY_REASONING_POLICY_BUILTIN_DEF,
    &LLM_MODEL_DEFAULTS_BUILTIN_DEF,
    &LLM_PROVIDER_CATALOG_BUILTIN_DEF,
    &LLM_PICK_MODEL_BUILTIN_DEF,
    &LLM_PROVIDERS_BUILTIN_DEF,
    &PROVIDER_REGISTER_BUILTIN_DEF,
    &LLM_CONFIG_BUILTIN_DEF,
    &LLM_CATALOG_BUILTIN_DEF,
    &LLM_PROVIDER_STATUS_BUILTIN_DEF,
    &LLM_RATE_LIMIT_BUILTIN_DEF,
    &LLM_HEALTHCHECK_BUILTIN_DEF,
];

/// Return provider/model capability metadata from the loaded capability matrix.
#[harn_builtin(
    sig = "provider_capabilities(provider: string, model?: string|nil) -> dict",
    category = "llm.config"
)]
fn provider_capabilities_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let provider = args.first().map(|a| a.display()).unwrap_or_default();
    let model = args.get(1).map(|a| a.display()).unwrap_or_default();
    if provider.is_empty() {
        return Err(VmError::Runtime(
            "provider_capabilities: provider name is required".to_string(),
        ));
    }
    let caps = super::capabilities::lookup(&provider, &model);
    Ok(capabilities_to_vm_value(&provider, &model, &caps))
}

/// Install raw TOML capability overrides for provider/model capability lookup.
#[harn_builtin(
    sig = "provider_capabilities_install(toml_src: string) -> bool",
    category = "llm.config"
)]
fn provider_capabilities_install_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let src = args.first().map(|a| a.display()).unwrap_or_default();
    if src.is_empty() {
        return Err(VmError::Runtime(
            "provider_capabilities_install: TOML source string required".to_string(),
        ));
    }
    super::capabilities::set_user_overrides_toml(&src).map_err(|e| {
        VmError::Runtime(format!("provider_capabilities_install: parse error: {e}"))
    })?;
    Ok(VmValue::Bool(true))
}

/// Clear installed provider/model capability overrides.
#[harn_builtin(sig = "provider_capabilities_clear() -> bool", category = "llm.config")]
fn provider_capabilities_clear_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    super::capabilities::clear_user_overrides();
    Ok(VmValue::Bool(true))
}

/// Infer the configured provider name for a model identifier.
#[harn_builtin(
    sig = "llm_infer_provider(model_id: string) -> string",
    category = "llm.config"
)]
fn llm_infer_provider_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let model_id = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(Rc::from(llm_config::infer_provider(
        &model_id,
    ))))
}

/// Return the configured capability tier for a model identifier.
#[harn_builtin(
    sig = "llm_model_tier(model_id: string) -> string",
    category = "llm.config"
)]
fn llm_model_tier_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let model_id = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(Rc::from(llm_config::model_tier(&model_id))))
}

/// Resolve a model alias or selector to full model metadata.
#[harn_builtin(
    sig = "llm_resolve_model(alias: string) -> dict",
    category = "llm.config"
)]
fn llm_resolve_model_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let alias = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(resolved_model_to_vm_value(&llm_config::resolve_model_info(
        &alias,
    )))
}

/// Return catalog metadata for a resolved model selector.
#[harn_builtin(
    sig = "llm_model_info(selector: string) -> dict",
    category = "llm.config"
)]
fn llm_model_info_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let selector = args.first().map(|a| a.display()).unwrap_or_default();
    let resolved = llm_config::resolve_model_info(&selector);
    Ok(model_info_to_vm_value(&resolved))
}

/// List configured model alias names.
#[harn_builtin(sig = "llm_known_models() -> list", category = "llm.config")]
fn llm_known_models_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(string_list_to_vm_value(llm_config::known_model_names()))
}

/// List providers usable in the current environment.
#[harn_builtin(sig = "llm_available_providers() -> list", category = "llm.config")]
fn llm_available_providers_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(string_list_to_vm_value(
        llm_config::available_provider_names(),
    ))
}

/// Return the configured cheap QC/repair model for a provider.
#[harn_builtin(
    sig = "llm_qc_default_model(provider: string) -> string|nil",
    category = "llm.config"
)]
fn llm_qc_default_model_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let provider = args.first().map(|a| a.display()).unwrap_or_default();
    if provider.is_empty() {
        return Err(VmError::Runtime(
            "llm_qc_default_model: provider name is required".to_string(),
        ));
    }
    Ok(llm_config::qc_default_model(&provider)
        .map(|model| VmValue::String(Rc::from(model)))
        .unwrap_or(VmValue::Nil))
}

/// Return glob-merged model_defaults for `model_id`.
#[harn_builtin(
    sig = "llm_model_defaults(model_id: string) -> dict",
    category = "llm.config"
)]
fn llm_model_defaults_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let model_id = args.first().map(|a| a.display()).unwrap_or_default();
    if model_id.is_empty() {
        return Err(VmError::Runtime(
            "llm_model_defaults: model_id is required".to_string(),
        ));
    }
    let params = llm_config::model_params(&model_id);
    let mut dict = BTreeMap::new();
    for (k, v) in &params {
        dict.insert(k.clone(), toml_value_to_vm_value(v));
    }
    Ok(VmValue::Dict(Rc::new(dict)))
}

/// Return the fully-merged llm_call options for `opts`. Requires opts.model.
#[harn_builtin(
    sig = "llm_resolved_options(opts: dict) -> dict",
    category = "llm.config"
)]
fn llm_resolved_options_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let opts = args
        .first()
        .and_then(|a| a.as_dict())
        .ok_or_else(|| VmError::Runtime("llm_resolved_options: opts must be a dict".to_string()))?;

    let model = opts
        .get("model")
        .map(|v| v.display())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            VmError::Runtime("llm_resolved_options: opts.model is required".to_string())
        })?;

    let user_provider = opts
        .get("provider")
        .map(|v| v.display())
        .filter(|s| !s.is_empty());

    let (resolved_id, provider_from_alias) = llm_config::resolve_model(&model);
    let final_provider = user_provider.unwrap_or_else(|| {
        provider_from_alias.unwrap_or_else(|| llm_config::infer_provider(&resolved_id))
    });

    let defaults = llm_config::model_params(&resolved_id);

    let mut out = opts.clone();
    for (k, v) in &defaults {
        if !out.contains_key(k) {
            out.insert(k.clone(), toml_value_to_vm_value(v));
        }
    }
    out.insert(
        "provider".to_string(),
        VmValue::String(Rc::from(final_provider)),
    );
    out.insert("model".to_string(), VmValue::String(Rc::from(resolved_id)));
    Ok(VmValue::Dict(Rc::new(out)))
}

/// Apply Harn's provider-aware reasoning_policy/thinking_policy defaults to an llm_call option dict.
#[harn_builtin(
    sig = "llm_apply_reasoning_policy(opts: dict) -> dict",
    category = "llm.config"
)]
fn llm_apply_reasoning_policy_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let opts = args.first().and_then(|a| a.as_dict()).ok_or_else(|| {
        VmError::Runtime("llm_apply_reasoning_policy: opts must be a dict".to_string())
    })?;
    let out = super::reasoning_policy::apply_policy_to_vm_options(opts)?;
    Ok(VmValue::Dict(Rc::new(out)))
}

fn toml_value_to_vm_value(value: &toml::Value) -> VmValue {
    match value {
        toml::Value::String(s) => VmValue::String(Rc::from(s.as_str())),
        toml::Value::Integer(i) => VmValue::Int(*i),
        toml::Value::Float(f) => VmValue::Float(*f),
        toml::Value::Boolean(b) => VmValue::Bool(*b),
        toml::Value::Datetime(dt) => VmValue::String(Rc::from(dt.to_string())),
        toml::Value::Array(items) => {
            let list: Vec<VmValue> = items.iter().map(toml_value_to_vm_value).collect();
            VmValue::List(Rc::new(list))
        }
        toml::Value::Table(table) => {
            let mut dict = BTreeMap::new();
            for (k, v) in table {
                dict.insert(k.clone(), toml_value_to_vm_value(v));
            }
            VmValue::Dict(Rc::new(dict))
        }
    }
}

/// Return the loaded provider, alias, model, pricing, and availability catalog.
#[harn_builtin(sig = "llm_provider_catalog() -> dict", category = "llm.config")]
fn llm_provider_catalog_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(provider_catalog_to_vm_value())
}

/// Resolve a model alias or tier to an `{id, provider, tier}` dict.
#[harn_builtin(
    sig = "llm_pick_model(target: string, options?: dict|nil) -> dict",
    category = "llm.config"
)]
fn llm_pick_model_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let target = args.first().map(|a| a.display()).unwrap_or_default();
    let options = args.get(1).and_then(|v| v.as_dict());
    let preferred_provider = options.and_then(|d| d.get("provider")).map(|v| v.display());

    let (id, provider) = if let Some((id, provider)) =
        llm_config::resolve_tier_model(&target, preferred_provider.as_deref())
    {
        (id, provider)
    } else {
        let (id, provider) = llm_config::resolve_model(&target);
        (
            id.clone(),
            provider.unwrap_or_else(|| llm_config::infer_provider(&id)),
        )
    };

    let mut dict = BTreeMap::new();
    dict.insert("id".to_string(), VmValue::String(Rc::from(id.clone())));
    dict.insert("provider".to_string(), VmValue::String(Rc::from(provider)));
    dict.insert(
        "tier".to_string(),
        VmValue::String(Rc::from(llm_config::model_tier(&id))),
    );
    Ok(VmValue::Dict(Rc::new(dict)))
}

/// List all configured and runtime-registered LLM provider names.
#[harn_builtin(sig = "llm_providers() -> list", category = "llm.config")]
fn llm_providers_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let config_names = llm_config::provider_names();
    let registry_names = super::provider::registered_provider_names();
    let mut all: std::collections::BTreeSet<String> = config_names.into_iter().collect();
    all.extend(registry_names);
    let list: Vec<VmValue> = all
        .into_iter()
        .map(|n| VmValue::String(Rc::from(n)))
        .collect();
    Ok(VmValue::List(Rc::new(list)))
}

/// Register a custom OpenAI-compatible provider name for runtime dispatch.
#[harn_builtin(
    sig = "provider_register(name: string) -> bool",
    category = "llm.config"
)]
fn provider_register_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let name = args.first().map(|a| a.display()).unwrap_or_default();
    if name.is_empty() {
        return Err(VmError::Runtime(
            "provider_register: name is required".to_string(),
        ));
    }
    super::provider::register_provider_name(&name);
    Ok(VmValue::Bool(true))
}

pub(crate) fn llm_catalog_value() -> VmValue {
    let entries: Vec<VmValue> = llm_config::model_catalog_entries()
        .into_iter()
        .map(|(id, model)| model_def_to_vm_value(&id, &model))
        .collect();
    VmValue::List(Rc::new(entries))
}

/// Return the full configured model catalog as a list of dicts:
/// `[{id, name, provider, context_window, runtime_context_window, capabilities,
/// quality_tags, pricing, availability, deprecated, deprecation_note, ...}, ...]`.
/// Alias for the read-only `harness.llm.catalog()` handle method, available for
/// scripts that do not receive a `Harness` parameter.
#[harn_builtin(sig = "llm_catalog() -> list", category = "llm.config")]
fn llm_catalog_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(llm_catalog_value())
}

pub(crate) fn llm_provider_status_value() -> VmValue {
    // Mirror `llm_providers()` for the name set so runtime-registered
    // providers also show up, but enrich each entry with a credential
    // probe so callers like `harn providers` and `harn doctor` can
    // render a single table without making N follow-up calls.
    //
    // Ensure the thread-local registered_provider_names() is populated
    // before snapshotting: callers like the CLI `run` path may invoke
    // this builtin before `reset_llm_state()` would have populated the
    // default set, which would silently omit `mock` (and any other
    // registered-only provider) from the status table.
    super::provider::register_default_providers();
    let mut names: std::collections::BTreeSet<String> =
        llm_config::provider_names().into_iter().collect();
    names.extend(super::provider::registered_provider_names());

    let mut entries = Vec::with_capacity(names.len());
    for name in names {
        let mut entry = BTreeMap::new();
        entry.insert("name".to_string(), VmValue::String(Rc::from(name.clone())));

        // Providers with `auth_style = "none"` (e.g. local Ollama) and
        // the multi-step auth providers (Bedrock/Vertex) report
        // `credential_status = "not_required"` / `"deferred"` rather
        // than `"ok"` so callers can distinguish a successfully
        // resolved API key from a deferred resolution. `mock` always
        // reports `"not_required"`.
        let (available, credential_status) = if name == "mock" {
            (true, "not_required")
        } else if matches!(name.as_str(), "bedrock" | "vertex") {
            (true, "deferred")
        } else if let Some(pdef) = llm_config::provider_config(&name) {
            if pdef.auth_style == "none" {
                (true, "not_required")
            } else {
                match super::helpers::resolve_api_key(&name) {
                    Ok(_) => (true, "ok"),
                    Err(_) => (false, "missing"),
                }
            }
        } else {
            // Runtime-registered providers without config entries fall
            // back to a credential probe through the standard helper.
            match super::helpers::resolve_api_key(&name) {
                Ok(_) => (true, "ok"),
                Err(_) => (false, "missing"),
            }
        };
        entry.insert("available".to_string(), VmValue::Bool(available));
        entry.insert(
            "credential_status".to_string(),
            VmValue::String(Rc::from(credential_status)),
        );
        entries.push(VmValue::Dict(Rc::new(entry)));
    }
    VmValue::List(Rc::new(entries))
}

/// Return a list of `{name, available, credential_status}` dicts describing every
/// configured provider plus runtime-registered names. `available` is true when
/// credentials resolve via the configured env vars (or when the provider uses
/// multi-step auth like Bedrock/Vertex). `credential_status` is one of
/// `"ok"`, `"missing"`, `"not_required"`, `"deferred"`. Alias for the
/// read-only `harness.llm.providers()` handle method, available for scripts that
/// do not receive a `Harness` parameter.
#[harn_builtin(sig = "llm_provider_status() -> list", category = "llm.config")]
fn llm_provider_status_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(llm_provider_status_value())
}

/// Return configured provider settings, or all provider settings when no provider is passed.
#[harn_builtin(
    sig = "llm_config(provider?: string|nil) -> dict|nil",
    category = "llm.config"
)]
fn llm_config_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let provider_name = args.first().map(|a| a.display());
    match provider_name {
        Some(name) => {
            if let Some(pdef) = llm_config::provider_config(&name) {
                Ok(provider_def_to_vm_value(Some(&name), &pdef))
            } else {
                Ok(VmValue::Nil)
            }
        }
        None => {
            let mut dict = BTreeMap::new();
            for name in llm_config::provider_names() {
                if let Some(pdef) = llm_config::provider_config(&name) {
                    dict.insert(name.clone(), provider_def_to_vm_value(Some(&name), &pdef));
                }
            }
            Ok(VmValue::Dict(Rc::new(dict)))
        }
    }
}

/// Set, query, or clear per-provider requests-per-minute rate limits.
#[harn_builtin(
    sig = "llm_rate_limit(provider: string, options?: dict|nil) -> bool|int|nil",
    category = "llm.rate_limit"
)]
fn llm_rate_limit_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let provider = args.first().map(|a| a.display()).unwrap_or_default();
    if provider.is_empty() {
        return Err(VmError::Runtime(
            "llm_rate_limit: provider name is required".to_string(),
        ));
    }
    if let Some(VmValue::Int(rpm)) = args
        .get(1)
        .and_then(|a| a.as_dict())
        .and_then(|o| o.get("rpm").cloned())
    {
        if rpm <= 0 {
            super::rate_limit::clear_rate_limit(&provider);
        } else {
            super::rate_limit::set_rate_limit(&provider, rpm as u32);
        }
        return Ok(VmValue::Bool(true));
    }
    if args.get(1).and_then(|a| a.as_dict()).is_some() {
        return Err(VmError::Runtime(
            "llm_rate_limit: options must include 'rpm' (integer)".to_string(),
        ));
    }
    match super::rate_limit::get_rate_limit(&provider) {
        Some(rpm) => Ok(VmValue::Int(rpm as i64)),
        None => Ok(VmValue::Nil),
    }
}

/// Validate provider health, API key reachability, and optional model readiness.
#[harn_builtin(
    sig = "llm_healthcheck(provider_or_options?: string|dict, options?: dict|nil) -> dict",
    kind = "async",
    category = "llm.config"
)]
async fn llm_healthcheck_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
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
            .or_else(|| std::env::var("HARN_LLM_MODEL").ok())
            .or_else(|| std::env::var("LOCAL_LLM_MODEL").ok());
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
            let mut readiness = super::api::OllamaReadinessOptions::new(resolved_model);
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
                    VmValue::String(raw) => super::api::normalize_ollama_keep_alive(raw)
                        .unwrap_or_else(|| vm_value_to_json(value)),
                    _ => vm_value_to_json(value),
                });
            let result = super::api::ollama_readiness(readiness).await;
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

    let requested_model =
        healthcheck_model_arg(&args).or_else(|| super::selected_model_for_provider(&provider_name));

    if let Some(model) = requested_model.filter(|model| !model.trim().is_empty()) {
        if let Some(pdef) = llm_config::provider_config(&provider_name) {
            if super::supports_model_readiness_probe(&pdef) {
                let key = api_key
                    .clone()
                    .or_else(|| super::resolve_api_key(&provider_name).ok())
                    .unwrap_or_default();
                let readiness =
                    super::probe_openai_compatible_model(&provider_name, &model, &key).await;
                return Ok(readiness_result(&readiness));
            }
        }
    }

    let result = super::run_provider_healthcheck_with_options(
        &provider_name,
        super::ProviderHealthcheckOptions {
            api_key,
            client: Some(super::shared_utility_client().clone()),
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

/// Convert a ProviderDef to a VmValue dict for the llm_config builtin.
fn provider_def_to_vm_value(
    provider_name: Option<&str>,
    pdef: &llm_config::ProviderDef,
) -> VmValue {
    let mut dict = BTreeMap::new();
    if let Some(display_name) = &pdef.display_name {
        dict.insert(
            "display_name".to_string(),
            VmValue::String(Rc::from(display_name.as_str())),
        );
    }
    if let Some(icon) = &pdef.icon {
        dict.insert("icon".to_string(), VmValue::String(Rc::from(icon.as_str())));
    }
    dict.insert(
        "base_url".to_string(),
        VmValue::String(Rc::from(pdef.base_url.as_str())),
    );
    if let Some(base_url_env) = &pdef.base_url_env {
        dict.insert(
            "base_url_env".to_string(),
            VmValue::String(Rc::from(base_url_env.as_str())),
        );
    }
    dict.insert(
        "auth_style".to_string(),
        VmValue::String(Rc::from(pdef.auth_style.as_str())),
    );
    dict.insert(
        "auth_envs".to_string(),
        string_list_to_vm_value(llm_config::auth_env_names(&pdef.auth_env)),
    );
    dict.insert(
        "auth_available".to_string(),
        VmValue::Bool(
            provider_name
                .map(llm_config::provider_key_available)
                .unwrap_or(pdef.auth_style == "none"),
        ),
    );
    dict.insert(
        "chat_endpoint".to_string(),
        VmValue::String(Rc::from(pdef.chat_endpoint.as_str())),
    );
    if let Some(endpoint) = &pdef.completion_endpoint {
        dict.insert(
            "completion_endpoint".to_string(),
            VmValue::String(Rc::from(endpoint.as_str())),
        );
    }
    if let Some(header) = &pdef.auth_header {
        dict.insert(
            "auth_header".to_string(),
            VmValue::String(Rc::from(header.as_str())),
        );
    }
    if !pdef.extra_headers.is_empty() {
        let mut headers = BTreeMap::new();
        for (k, v) in &pdef.extra_headers {
            headers.insert(k.clone(), VmValue::String(Rc::from(v.as_str())));
        }
        dict.insert("extra_headers".to_string(), VmValue::Dict(Rc::new(headers)));
    }
    if !pdef.features.is_empty() {
        let features: Vec<VmValue> = pdef
            .features
            .iter()
            .map(|f| VmValue::String(Rc::from(f.as_str())))
            .collect();
        dict.insert("features".to_string(), VmValue::List(Rc::new(features)));
    }
    if let Some(rpm) = pdef.rpm {
        dict.insert("rpm".to_string(), VmValue::Int(rpm as i64));
    }
    if let Some(cost) = pdef.cost_per_1k_in {
        dict.insert("cost_per_1k_in".to_string(), VmValue::Float(cost));
    }
    if let Some(cost) = pdef.cost_per_1k_out {
        dict.insert("cost_per_1k_out".to_string(), VmValue::Float(cost));
    }
    if let Some(latency) = pdef.latency_p50_ms {
        dict.insert("latency_p50_ms".to_string(), VmValue::Int(latency as i64));
    }
    VmValue::Dict(Rc::new(dict))
}

fn string_list_to_vm_value(items: Vec<String>) -> VmValue {
    VmValue::List(Rc::new(
        items
            .into_iter()
            .map(|item| VmValue::String(Rc::from(item)))
            .collect(),
    ))
}

fn resolved_model_to_vm_value(resolved: &llm_config::ResolvedModel) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert(
        "id".to_string(),
        VmValue::String(Rc::from(resolved.id.as_str())),
    );
    dict.insert(
        "provider".to_string(),
        VmValue::String(Rc::from(resolved.provider.as_str())),
    );
    dict.insert(
        "alias".to_string(),
        resolved
            .alias
            .as_deref()
            .map(|alias| VmValue::String(Rc::from(alias)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "tool_format".to_string(),
        VmValue::String(Rc::from(resolved.tool_format.as_str())),
    );
    dict.insert(
        "tier".to_string(),
        VmValue::String(Rc::from(resolved.tier.as_str())),
    );
    VmValue::Dict(Rc::new(dict))
}

fn model_info_to_vm_value(resolved: &llm_config::ResolvedModel) -> VmValue {
    let mut dict = match resolved_model_to_vm_value(resolved) {
        VmValue::Dict(dict) => dict.as_ref().clone(),
        _ => unreachable!("resolved_model_to_vm_value returns a dict"),
    };
    let caps = super::capabilities::lookup(&resolved.provider, &resolved.id);
    dict.insert(
        "capabilities".to_string(),
        capabilities_to_vm_value(&resolved.provider, &resolved.id, &caps),
    );
    dict.insert(
        "catalog".to_string(),
        llm_config::model_catalog_entry(&resolved.id)
            .map(|entry| model_def_to_vm_value(&resolved.id, &entry))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "qc_default_model".to_string(),
        llm_config::qc_default_model(&resolved.provider)
            .map(|model| VmValue::String(Rc::from(model)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "auth_available".to_string(),
        VmValue::Bool(llm_config::provider_key_available(&resolved.provider)),
    );
    VmValue::Dict(Rc::new(dict))
}

pub(crate) fn capabilities_to_vm_value(
    provider: &str,
    model: &str,
    caps: &super::capabilities::Capabilities,
) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert(
        "provider".to_string(),
        VmValue::String(Rc::from(provider.to_string())),
    );
    dict.insert(
        "model".to_string(),
        VmValue::String(Rc::from(model.to_string())),
    );
    dict.insert("native_tools".to_string(), VmValue::Bool(caps.native_tools));
    dict.insert(
        "message_wire_format".to_string(),
        VmValue::String(Rc::from(caps.message_wire_format.clone())),
    );
    dict.insert(
        "native_tool_wire_format".to_string(),
        VmValue::String(Rc::from(caps.native_tool_wire_format.clone())),
    );
    dict.insert(
        "text_tool_wire_format_supported".to_string(),
        VmValue::Bool(caps.text_tool_wire_format_supported),
    );
    dict.insert(
        "preferred_tool_format".to_string(),
        caps.preferred_tool_format
            .as_deref()
            .map(|format| VmValue::String(Rc::from(format)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "tool_mode_parity".to_string(),
        caps.tool_mode_parity
            .as_deref()
            .map(|status| VmValue::String(Rc::from(status)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "tool_mode_parity_notes".to_string(),
        caps.tool_mode_parity_notes
            .as_deref()
            .map(|notes| VmValue::String(Rc::from(notes)))
            .unwrap_or(VmValue::Nil),
    );
    // Mirrors the VM's tool-capability gate at llm_config::effective_model_capability_tags:
    // either native or text-format tool calling counts as tool-capable.
    dict.insert(
        "tools".to_string(),
        VmValue::Bool(caps.native_tools || caps.text_tool_wire_format_supported),
    );
    dict.insert(
        "defer_loading".to_string(),
        VmValue::Bool(caps.defer_loading),
    );
    dict.insert(
        "tool_search".to_string(),
        string_list_to_vm_value(caps.tool_search.clone()),
    );
    dict.insert(
        "responses_api".to_string(),
        VmValue::Bool(caps.responses_api),
    );
    dict.insert(
        "hosted_tools".to_string(),
        string_list_to_vm_value(caps.hosted_tools.clone()),
    );
    dict.insert("remote_mcp".to_string(), VmValue::Bool(caps.remote_mcp));
    dict.insert(
        "conversation_state".to_string(),
        VmValue::Bool(caps.conversation_state),
    );
    dict.insert("compaction".to_string(), VmValue::Bool(caps.compaction));
    dict.insert(
        "background_mode".to_string(),
        VmValue::Bool(caps.background_mode),
    );
    dict.insert(
        "tool_approval_policy".to_string(),
        caps.tool_approval_policy
            .as_deref()
            .map(|value| VmValue::String(Rc::from(value)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "max_tools".to_string(),
        caps.max_tools
            .map(|n| VmValue::Int(n as i64))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "prompt_caching".to_string(),
        VmValue::Bool(caps.prompt_caching),
    );
    dict.insert(
        "prefers_xml_scaffolding".to_string(),
        VmValue::Bool(caps.prefers_xml_scaffolding),
    );
    dict.insert(
        "prefers_markdown_scaffolding".to_string(),
        VmValue::Bool(caps.prefers_markdown_scaffolding),
    );
    dict.insert(
        "structured_output_mode".to_string(),
        VmValue::String(Rc::from(caps.structured_output_mode.as_str())),
    );
    dict.insert(
        "supports_assistant_prefill".to_string(),
        VmValue::Bool(caps.supports_assistant_prefill),
    );
    dict.insert(
        "prefers_role_developer".to_string(),
        VmValue::Bool(caps.prefers_role_developer),
    );
    dict.insert(
        "prefers_xml_tools".to_string(),
        VmValue::Bool(caps.prefers_xml_tools),
    );
    dict.insert(
        "thinking".to_string(),
        VmValue::Bool(!caps.thinking_modes.is_empty()),
    );
    dict.insert(
        "thinking_block_style".to_string(),
        VmValue::String(Rc::from(caps.thinking_block_style.as_str())),
    );
    dict.insert(
        "thinking_modes".to_string(),
        string_list_to_vm_value(caps.thinking_modes.clone()),
    );
    dict.insert(
        "interleaved_thinking_supported".to_string(),
        VmValue::Bool(caps.interleaved_thinking_supported),
    );
    dict.insert(
        "anthropic_beta_features".to_string(),
        string_list_to_vm_value(caps.anthropic_beta_features.clone()),
    );
    dict.insert(
        "vision_supported".to_string(),
        VmValue::Bool(caps.vision_supported),
    );
    dict.insert("audio".to_string(), VmValue::Bool(caps.audio));
    dict.insert("pdf".to_string(), VmValue::Bool(caps.pdf));
    dict.insert(
        "files_api_supported".to_string(),
        VmValue::Bool(caps.files_api_supported),
    );
    dict.insert(
        "file_upload_wire_format".to_string(),
        caps.file_upload_wire_format
            .as_ref()
            .map(|value| VmValue::String(Rc::from(value.clone())))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "structured_output".to_string(),
        caps.structured_output
            .as_deref()
            .map(|value| VmValue::String(Rc::from(value)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "json_schema".to_string(),
        caps.json_schema
            .as_deref()
            .map(|value| VmValue::String(Rc::from(value)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "prefers_xml_scaffolding".to_string(),
        VmValue::Bool(caps.prefers_xml_scaffolding),
    );
    dict.insert(
        "prefers_markdown_scaffolding".to_string(),
        VmValue::Bool(caps.prefers_markdown_scaffolding),
    );
    dict.insert(
        "structured_output_mode".to_string(),
        VmValue::String(Rc::from(caps.structured_output_mode.as_str())),
    );
    dict.insert(
        "supports_assistant_prefill".to_string(),
        VmValue::Bool(caps.supports_assistant_prefill),
    );
    dict.insert(
        "prefers_role_developer".to_string(),
        VmValue::Bool(caps.prefers_role_developer),
    );
    dict.insert(
        "prefers_xml_tools".to_string(),
        VmValue::Bool(caps.prefers_xml_tools),
    );
    dict.insert(
        "thinking_block_style".to_string(),
        VmValue::String(Rc::from(caps.thinking_block_style.as_str())),
    );
    dict.insert(
        "preserve_thinking".to_string(),
        VmValue::Bool(caps.preserve_thinking),
    );
    dict.insert(
        "requires_completion_tokens".to_string(),
        VmValue::Bool(caps.requires_completion_tokens),
    );
    dict.insert(
        "reasoning_effort_supported".to_string(),
        VmValue::Bool(caps.reasoning_effort_supported),
    );
    dict.insert(
        "reasoning_none_supported".to_string(),
        VmValue::Bool(caps.reasoning_none_supported),
    );
    dict.insert(
        "reasoning_wire_format".to_string(),
        caps.reasoning_wire_format
            .as_ref()
            .map(|value| VmValue::String(Rc::from(value.clone())))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "seed_supported".to_string(),
        VmValue::Bool(caps.seed_supported),
    );
    dict.insert(
        "top_k_supported".to_string(),
        VmValue::Bool(caps.top_k_supported),
    );
    dict.insert(
        "frequency_penalty_supported".to_string(),
        VmValue::Bool(caps.frequency_penalty_supported),
    );
    dict.insert(
        "presence_penalty_supported".to_string(),
        VmValue::Bool(caps.presence_penalty_supported),
    );
    VmValue::Dict(Rc::new(dict))
}

fn model_def_to_vm_value(id: &str, model: &llm_config::ModelDef) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("id".to_string(), VmValue::String(Rc::from(id.to_string())));
    dict.insert(
        "name".to_string(),
        VmValue::String(Rc::from(model.name.as_str())),
    );
    dict.insert(
        "provider".to_string(),
        VmValue::String(Rc::from(model.provider.as_str())),
    );
    dict.insert(
        "context_window".to_string(),
        VmValue::Int(model.context_window as i64),
    );
    dict.insert(
        "runtime_context_window".to_string(),
        model
            .runtime_context_window
            .map(|window| VmValue::Int(window as i64))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "stream_timeout".to_string(),
        model
            .stream_timeout
            .map(VmValue::Float)
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "capabilities".to_string(),
        string_list_to_vm_value(model.capabilities.clone()),
    );
    dict.insert(
        "pricing".to_string(),
        model
            .pricing
            .as_ref()
            .map(pricing_to_vm_value)
            .unwrap_or(VmValue::Nil),
    );
    dict.insert("deprecated".to_string(), VmValue::Bool(model.deprecated));
    dict.insert(
        "deprecation_note".to_string(),
        model
            .deprecation_note
            .as_deref()
            .map(|note| VmValue::String(Rc::from(note)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "quality_tags".to_string(),
        string_list_to_vm_value(model.quality_tags.clone()),
    );
    dict.insert(
        "availability".to_string(),
        VmValue::String(Rc::from(model.availability.as_str())),
    );
    dict.insert(
        "tier".to_string(),
        VmValue::String(Rc::from(llm_config::model_tier(id))),
    );
    dict.insert(
        "open_weight".to_string(),
        model.open_weight.map(VmValue::Bool).unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "strengths".to_string(),
        string_list_to_vm_value(model.strengths.clone()),
    );
    let benchmarks: BTreeMap<String, VmValue> = model
        .benchmarks
        .iter()
        .map(|(key, value)| (key.clone(), VmValue::Float(*value)))
        .collect();
    dict.insert("benchmarks".to_string(), VmValue::Dict(Rc::new(benchmarks)));
    VmValue::Dict(Rc::new(dict))
}

fn pricing_to_vm_value(pricing: &llm_config::ModelPricing) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert(
        "input_per_mtok".to_string(),
        VmValue::Float(pricing.input_per_mtok),
    );
    dict.insert(
        "output_per_mtok".to_string(),
        VmValue::Float(pricing.output_per_mtok),
    );
    dict.insert(
        "cache_read_per_mtok".to_string(),
        pricing
            .cache_read_per_mtok
            .map(VmValue::Float)
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "cache_write_per_mtok".to_string(),
        pricing
            .cache_write_per_mtok
            .map(VmValue::Float)
            .unwrap_or(VmValue::Nil),
    );
    VmValue::Dict(Rc::new(dict))
}

fn provider_catalog_to_vm_value() -> VmValue {
    let mut dict = BTreeMap::new();

    let mut providers = Vec::new();
    for name in llm_config::provider_names() {
        if let Some(pdef) = llm_config::provider_config(&name) {
            let mut provider = match provider_def_to_vm_value(Some(&name), &pdef) {
                VmValue::Dict(provider) => provider.as_ref().clone(),
                _ => unreachable!("provider_def_to_vm_value returns a dict"),
            };
            provider.insert("name".to_string(), VmValue::String(Rc::from(name.clone())));
            providers.push(VmValue::Dict(Rc::new(provider)));
        }
    }
    dict.insert("providers".to_string(), VmValue::List(Rc::new(providers)));
    dict.insert(
        "known_model_names".to_string(),
        string_list_to_vm_value(llm_config::known_model_names()),
    );
    dict.insert(
        "available_providers".to_string(),
        string_list_to_vm_value(llm_config::available_provider_names()),
    );
    let aliases = llm_config::alias_entries()
        .into_iter()
        .map(|(name, alias)| alias_def_to_vm_value(&name, &alias))
        .collect();
    dict.insert("aliases".to_string(), VmValue::List(Rc::new(aliases)));
    let models = llm_config::model_catalog_entries()
        .into_iter()
        .map(|(id, model)| model_def_to_vm_value(&id, &model))
        .collect();
    dict.insert("models".to_string(), VmValue::List(Rc::new(models)));
    let qc_defaults = llm_config::qc_defaults()
        .into_iter()
        .map(|(provider, model)| (provider, VmValue::String(Rc::from(model))))
        .collect();
    dict.insert(
        "qc_defaults".to_string(),
        VmValue::Dict(Rc::new(qc_defaults)),
    );

    VmValue::Dict(Rc::new(dict))
}

fn alias_def_to_vm_value(name: &str, alias: &llm_config::AliasDef) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert(
        "name".to_string(),
        VmValue::String(Rc::from(name.to_string())),
    );
    dict.insert(
        "id".to_string(),
        VmValue::String(Rc::from(alias.id.as_str())),
    );
    dict.insert(
        "provider".to_string(),
        VmValue::String(Rc::from(alias.provider.as_str())),
    );
    dict.insert(
        "tool_format".to_string(),
        alias
            .tool_format
            .as_deref()
            .map(|format| VmValue::String(Rc::from(format)))
            .unwrap_or(VmValue::Nil),
    );
    VmValue::Dict(Rc::new(dict))
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

fn readiness_result(readiness: &super::ModelReadiness) -> VmValue {
    let mut meta = BTreeMap::new();
    meta.insert(
        "category".to_string(),
        VmValue::String(Rc::from(readiness.category.as_str())),
    );
    meta.insert(
        "provider".to_string(),
        VmValue::String(Rc::from(readiness.provider.as_str())),
    );
    meta.insert(
        "model".to_string(),
        VmValue::String(Rc::from(readiness.model.as_str())),
    );
    meta.insert(
        "url".to_string(),
        readiness
            .url
            .as_ref()
            .map(|url| VmValue::String(Rc::from(url.as_str())))
            .unwrap_or(VmValue::Nil),
    );
    meta.insert(
        "status".to_string(),
        readiness
            .status
            .map(|status| VmValue::Int(status as i64))
            .unwrap_or(VmValue::Nil),
    );
    meta.insert(
        "available_models".to_string(),
        VmValue::List(Rc::new(
            readiness
                .available_models
                .iter()
                .map(|model| VmValue::String(Rc::from(model.as_str())))
                .collect(),
        )),
    );
    healthcheck_result_with_meta(readiness.valid, &readiness.message, meta)
}

/// Build a healthcheck result dict with optional metadata.
fn healthcheck_result_with_meta(
    valid: bool,
    message: &str,
    meta: BTreeMap<String, VmValue>,
) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("valid".to_string(), VmValue::Bool(valid));
    dict.insert("message".to_string(), VmValue::String(Rc::from(message)));
    dict.insert("metadata".to_string(), VmValue::Dict(Rc::new(meta)));
    VmValue::Dict(Rc::new(dict))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_dict(entries: Vec<(&str, VmValue)>) -> VmValue {
        let mut map = BTreeMap::new();
        for (k, v) in entries {
            map.insert(k.to_string(), v);
        }
        VmValue::Dict(Rc::new(map))
    }

    #[test]
    fn test_llm_model_defaults_returns_empty_for_unknown_model() {
        llm_config::clear_user_overrides();
        let mut out = String::new();
        let args = vec![VmValue::String(Rc::from(
            "definitely-not-a-real-model-id-zzzzz",
        ))];
        let result = llm_model_defaults_builtin(&args, &mut out).expect("builtin returned error");
        let dict = result.as_dict().expect("expected dict");
        assert!(
            dict.is_empty(),
            "unknown model should yield empty defaults dict, got {dict:?}"
        );
    }

    #[test]
    fn test_llm_resolved_options_requires_model() {
        llm_config::clear_user_overrides();
        let mut out = String::new();
        let args = vec![build_dict(vec![])];
        let err =
            llm_resolved_options_builtin(&args, &mut out).expect_err("missing model should error");
        match err {
            VmError::Runtime(message) => {
                assert!(
                    message.contains("opts.model is required"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected Runtime error, got {other:?}"),
        }
    }

    #[test]
    fn test_llm_resolved_options_user_wins_over_defaults() {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        llm_config::clear_user_overrides();
        let mut overlay = llm_config::ProvidersConfig::default();
        let mut model_defaults = BTreeMap::new();
        model_defaults.insert(
            "fake-resolved-options-model".to_string(),
            toml::Value::Float(0.5),
        );
        overlay
            .model_defaults
            .insert("fake-resolved-options-model".to_string(), model_defaults);
        llm_config::set_user_overrides(Some(overlay));

        let mut out = String::new();
        let args = vec![build_dict(vec![
            (
                "model",
                VmValue::String(Rc::from("fake-resolved-options-model")),
            ),
            ("temperature", VmValue::Float(0.9)),
        ])];
        let result = llm_resolved_options_builtin(&args, &mut out).expect("builtin returned error");
        let dict = result.as_dict().expect("expected dict");
        match dict.get("temperature") {
            Some(VmValue::Float(f)) => assert!((*f - 0.9).abs() < 1e-9, "user value lost: {f}"),
            other => panic!("expected Float(0.9), got {other:?}"),
        }
        match dict.get("model") {
            Some(VmValue::String(s)) => assert_eq!(s.as_ref(), "fake-resolved-options-model"),
            other => panic!("expected model string, got {other:?}"),
        }

        llm_config::clear_user_overrides();
    }

    #[test]
    fn test_llm_resolved_options_default_fills_unspecified() {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        llm_config::clear_user_overrides();
        let mut overlay = llm_config::ProvidersConfig::default();
        let mut model_defaults = BTreeMap::new();
        model_defaults.insert("temperature".to_string(), toml::Value::Float(0.5));
        overlay
            .model_defaults
            .insert("fake-fill-defaults-model".to_string(), model_defaults);
        llm_config::set_user_overrides(Some(overlay));

        let mut out = String::new();
        let args = vec![build_dict(vec![(
            "model",
            VmValue::String(Rc::from("fake-fill-defaults-model")),
        )])];
        let result = llm_resolved_options_builtin(&args, &mut out).expect("builtin returned error");
        let dict = result.as_dict().expect("expected dict");
        match dict.get("temperature") {
            Some(VmValue::Float(f)) => assert!((*f - 0.5).abs() < 1e-9, "default lost: {f}"),
            other => panic!("expected Float(0.5), got {other:?}"),
        }

        llm_config::clear_user_overrides();
    }

    #[test]
    fn test_llm_resolved_options_resolves_provider() {
        let _guard = crate::llm::env_lock().lock().expect("env lock");
        let prev_default_provider = std::env::var("HARN_DEFAULT_PROVIDER").ok();
        unsafe {
            std::env::remove_var("HARN_DEFAULT_PROVIDER");
        }
        llm_config::clear_user_overrides();

        let mut out = String::new();
        let args = vec![build_dict(vec![(
            "model",
            VmValue::String(Rc::from("claude-sonnet-4-20250514")),
        )])];
        let result = llm_resolved_options_builtin(&args, &mut out).expect("builtin returned error");
        let dict = result.as_dict().expect("expected dict");
        match dict.get("provider") {
            Some(VmValue::String(s)) => {
                assert_eq!(s.as_ref(), "anthropic", "provider mismatch: {s}");
            }
            other => panic!("expected provider string, got {other:?}"),
        }

        unsafe {
            match prev_default_provider {
                Some(value) => std::env::set_var("HARN_DEFAULT_PROVIDER", value),
                None => std::env::remove_var("HARN_DEFAULT_PROVIDER"),
            }
        }
    }

    #[test]
    fn test_provider_capabilities_exposes_tools_for_text_only_models() {
        super::super::capabilities::clear_user_overrides();
        let mut out = String::new();
        let args = vec![
            VmValue::String(Rc::from("ollama")),
            VmValue::String(Rc::from("qwen3.6:35b-a3b-coding-nvfp4")),
        ];
        let result =
            provider_capabilities_builtin(&args, &mut out).expect("builtin returned error");
        let dict = result.as_dict().expect("expected dict");
        // qwen3.6 on Ollama uses Harn's text tool-call wire format, not native API tools.
        // Scripts gating on `tools` should still see it as tool-capable.
        let expect_bool = |key: &str, want: bool| match dict.get(key) {
            Some(VmValue::Bool(b)) => assert_eq!(*b, want, "{key}"),
            other => panic!("expected Bool for {key}, got {other:?}"),
        };
        expect_bool("native_tools", false);
        expect_bool("text_tool_wire_format_supported", true);
        expect_bool("tools", true);
        expect_bool("prefers_markdown_scaffolding", true);
        expect_bool("prefers_xml_tools", false);
        match dict.get("structured_output_mode") {
            Some(VmValue::String(mode)) => assert_eq!(mode.as_ref(), "delimited"),
            other => panic!("expected structured_output_mode string, got {other:?}"),
        }
        match dict.get("thinking_block_style") {
            Some(VmValue::String(style)) => assert_eq!(style.as_ref(), "inline"),
            other => panic!("expected thinking_block_style string, got {other:?}"),
        }
    }

    #[test]
    fn test_provider_capabilities_exposes_prompt_format_preferences() {
        super::super::capabilities::clear_user_overrides();
        let mut out = String::new();
        let args = vec![
            VmValue::String(Rc::from("anthropic")),
            VmValue::String(Rc::from("claude-opus-4-7")),
        ];
        let result =
            provider_capabilities_builtin(&args, &mut out).expect("builtin returned error");
        let dict = result.as_dict().expect("expected dict");

        let expect_bool = |key: &str, want: bool| match dict.get(key) {
            Some(VmValue::Bool(b)) => assert_eq!(*b, want, "{key}"),
            other => panic!("expected Bool for {key}, got {other:?}"),
        };
        let expect_string = |key: &str, want: &str| match dict.get(key) {
            Some(VmValue::String(value)) => assert_eq!(value.as_ref(), want, "{key}"),
            other => panic!("expected String for {key}, got {other:?}"),
        };

        expect_bool("prefers_xml_scaffolding", true);
        expect_bool("prefers_xml_tools", true);
        expect_bool("supports_assistant_prefill", false);
        expect_string("structured_output_mode", "xml_tagged");
        expect_string("thinking_block_style", "thinking_blocks");
    }
}

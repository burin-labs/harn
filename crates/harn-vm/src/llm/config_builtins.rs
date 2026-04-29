use std::collections::BTreeMap;
use std::rc::Rc;

use crate::llm_config;
use crate::stdlib::json_to_vm_value;
use crate::value::VmValue;
use crate::vm::Vm;

use super::helpers::vm_value_to_json;

/// Register config-based LLM builtins (llm_infer_provider, llm_resolve_model, etc.).
pub(crate) fn register_config_builtins(vm: &mut Vm) {
    vm.register_builtin("provider_capabilities", |args, _out| {
        // provider_capabilities(provider, model) -> dict of capabilities.
        //
        // Returns a dict with every capability the (provider, model)
        // pair advertises in the loaded matrix. Scripts can branch on
        // the returned values without caring which vendor they're
        // pointed at (e.g. `if "bm25" in caps.tool_search { ... }`).
        //
        // Unknown provider/model pairs return an all-default dict
        // (everything off, empty tool_search, no max_tools). This is
        // the same shape the defaults trait impl uses.
        let provider = args.first().map(|a| a.display()).unwrap_or_default();
        let model = args.get(1).map(|a| a.display()).unwrap_or_default();
        if provider.is_empty() {
            return Err(crate::value::VmError::Runtime(
                "provider_capabilities: provider name is required".to_string(),
            ));
        }
        let caps = super::capabilities::lookup(&provider, &model);
        Ok(capabilities_to_vm_value(&provider, &model, &caps))
    });

    // provider_capabilities_install(toml_src) — install capability
    // overrides from a raw TOML source (same layout as the shipped
    // `capabilities.toml`: top-level `[[provider.<name>]]` arrays plus
    // an optional `[provider_family]` table). Mirrors harn.toml's
    // `[capabilities]` section but in-script, so conformance tests and
    // scripts that autodetect proxied endpoints can exercise the
    // override path without editing the manifest. Returns true on
    // success, throws a runtime error on parse failure.
    vm.register_builtin("provider_capabilities_install", |args, _out| {
        let src = args.first().map(|a| a.display()).unwrap_or_default();
        if src.is_empty() {
            return Err(crate::value::VmError::Runtime(
                "provider_capabilities_install: TOML source string required".to_string(),
            ));
        }
        super::capabilities::set_user_overrides_toml(&src).map_err(|e| {
            crate::value::VmError::Runtime(format!(
                "provider_capabilities_install: parse error: {e}"
            ))
        })?;
        Ok(VmValue::Bool(true))
    });

    vm.register_builtin("provider_capabilities_clear", |_args, _out| {
        super::capabilities::clear_user_overrides();
        Ok(VmValue::Bool(true))
    });

    vm.register_builtin("llm_infer_provider", |args, _out| {
        let model_id = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(llm_config::infer_provider(
            &model_id,
        ))))
    });

    vm.register_builtin("llm_model_tier", |args, _out| {
        let model_id = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(llm_config::model_tier(&model_id))))
    });

    vm.register_builtin("llm_resolve_model", |args, _out| {
        let alias = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(resolved_model_to_vm_value(&llm_config::resolve_model_info(
            &alias,
        )))
    });

    vm.register_builtin("llm_model_info", |args, _out| {
        let selector = args.first().map(|a| a.display()).unwrap_or_default();
        let resolved = llm_config::resolve_model_info(&selector);
        Ok(model_info_to_vm_value(&resolved))
    });

    vm.register_builtin("llm_known_models", |_args, _out| {
        Ok(string_list_to_vm_value(llm_config::known_model_names()))
    });

    vm.register_builtin("llm_available_providers", |_args, _out| {
        Ok(string_list_to_vm_value(
            llm_config::available_provider_names(),
        ))
    });

    vm.register_builtin("llm_qc_default_model", |args, _out| {
        let provider = args.first().map(|a| a.display()).unwrap_or_default();
        if provider.is_empty() {
            return Err(crate::value::VmError::Runtime(
                "llm_qc_default_model: provider name is required".to_string(),
            ));
        }
        Ok(llm_config::qc_default_model(&provider)
            .map(|model| VmValue::String(Rc::from(model)))
            .unwrap_or(VmValue::Nil))
    });

    vm.register_builtin("llm_provider_catalog", |_args, _out| {
        Ok(provider_catalog_to_vm_value())
    });

    vm.register_builtin("llm_pick_model", |args, _out| {
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
        dict.insert(
            "provider".to_string(),
            VmValue::String(Rc::from(provider.clone())),
        );
        dict.insert(
            "tier".to_string(),
            VmValue::String(Rc::from(llm_config::model_tier(&id))),
        );
        Ok(VmValue::Dict(Rc::new(dict)))
    });

    vm.register_builtin("llm_providers", |_args, _out| {
        let config_names = llm_config::provider_names();
        let registry_names = super::provider::registered_provider_names();
        // Merge config-defined and registry-defined provider names
        let mut all: std::collections::BTreeSet<String> = config_names.into_iter().collect();
        all.extend(registry_names);
        let list: Vec<VmValue> = all
            .into_iter()
            .map(|n| VmValue::String(Rc::from(n)))
            .collect();
        Ok(VmValue::List(Rc::new(list)))
    });

    // provider_register — register a custom provider name at runtime so
    // `llm_call` can dispatch to it. The provider must be OpenAI-compatible
    // and configured via llm_config or environment variables.
    vm.register_builtin("provider_register", |args, _out| {
        let name = args.first().map(|a| a.display()).unwrap_or_default();
        if name.is_empty() {
            return Err(crate::value::VmError::Runtime(
                "provider_register: name is required".to_string(),
            ));
        }
        super::provider::register_provider_name(&name);
        Ok(VmValue::Bool(true))
    });

    vm.register_builtin("llm_config", |args, _out| {
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
                // Return all providers as a dict
                let mut dict = BTreeMap::new();
                for name in llm_config::provider_names() {
                    if let Some(pdef) = llm_config::provider_config(&name) {
                        dict.insert(name.clone(), provider_def_to_vm_value(Some(&name), &pdef));
                    }
                }
                Ok(VmValue::Dict(Rc::new(dict)))
            }
        }
    });

    // llm_rate_limit — set, query, or clear per-provider RPM rate limits.
    //   llm_rate_limit("together", {rpm: 600})  -> set
    //   llm_rate_limit("together")              -> query (returns Int or Nil)
    //   llm_rate_limit("together", {rpm: 0})    -> clear
    vm.register_builtin("llm_rate_limit", |args, _out| {
        let provider = args.first().map(|a| a.display()).unwrap_or_default();
        if provider.is_empty() {
            return Err(crate::value::VmError::Runtime(
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
            return Err(crate::value::VmError::Runtime(
                "llm_rate_limit: options must include 'rpm' (integer)".to_string(),
            ));
        }
        // Query mode
        match super::rate_limit::get_rate_limit(&provider) {
            Some(rpm) => Ok(VmValue::Int(rpm as i64)),
            None => Ok(VmValue::Nil),
        }
    });

    vm.register_async_builtin("llm_healthcheck", |args| async move {
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
                readiness.base_url = options
                    .and_then(|opts| opts.get("base_url").or_else(|| opts.get("url")))
                    .map(|value| value.display())
                    .filter(|value| !value.trim().is_empty());
                readiness.keep_alive =
                    options
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

        let requested_model = healthcheck_model_arg(&args)
            .or_else(|| super::selected_model_for_provider(&provider_name));

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
        let json = serde_json::to_value(result).map_err(|error| {
            crate::value::VmError::Runtime(format!("llm_healthcheck: serialize result: {error}"))
        })?;
        Ok(crate::schema::json_to_vm_value(&json))
    });
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

fn capabilities_to_vm_value(
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
        "defer_loading".to_string(),
        VmValue::Bool(caps.defer_loading),
    );
    dict.insert(
        "tool_search".to_string(),
        string_list_to_vm_value(caps.tool_search.clone()),
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
        "thinking".to_string(),
        VmValue::Bool(!caps.thinking_modes.is_empty()),
    );
    dict.insert(
        "thinking_modes".to_string(),
        string_list_to_vm_value(caps.thinking_modes.clone()),
    );
    dict.insert(
        "preserve_thinking".to_string(),
        VmValue::Bool(caps.preserve_thinking),
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

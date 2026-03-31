use std::collections::BTreeMap;
use std::rc::Rc;

use crate::llm_config;
use crate::value::VmValue;
use crate::vm::Vm;

use super::api::apply_auth_headers;
use super::helpers::vm_resolve_api_key;

/// Register config-based LLM builtins (llm_infer_provider, llm_resolve_model, etc.).
pub(crate) fn register_config_builtins(vm: &mut Vm) {
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
        let (id, provider) = llm_config::resolve_model(&alias);
        let mut dict = BTreeMap::new();
        dict.insert("id".to_string(), VmValue::String(Rc::from(id)));
        dict.insert(
            "provider".to_string(),
            provider
                .map(|p| VmValue::String(Rc::from(p)))
                .unwrap_or(VmValue::Nil),
        );
        Ok(VmValue::Dict(Rc::new(dict)))
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
        let names = llm_config::provider_names();
        let list: Vec<VmValue> = names
            .into_iter()
            .map(|n| VmValue::String(Rc::from(n)))
            .collect();
        Ok(VmValue::List(Rc::new(list)))
    });

    vm.register_builtin("llm_config", |args, _out| {
        let provider_name = args.first().map(|a| a.display());
        match provider_name {
            Some(name) => {
                if let Some(pdef) = llm_config::provider_config(&name) {
                    Ok(provider_def_to_vm_value(pdef))
                } else {
                    Ok(VmValue::Nil)
                }
            }
            None => {
                // Return all providers as a dict
                let mut dict = BTreeMap::new();
                for name in llm_config::provider_names() {
                    if let Some(pdef) = llm_config::provider_config(&name) {
                        dict.insert(name, provider_def_to_vm_value(pdef));
                    }
                }
                Ok(VmValue::Dict(Rc::new(dict)))
            }
        }
    });

    vm.register_async_builtin("llm_healthcheck", |args| async move {
        let provider_name = args
            .first()
            .map(|a| a.display())
            .unwrap_or_else(|| "anthropic".to_string());

        let api_key = vm_resolve_api_key(&provider_name).unwrap_or_default();

        let pdef = match llm_config::provider_config(&provider_name) {
            Some(p) => p,
            None => {
                return Ok(healthcheck_result(
                    false,
                    &format!("Unknown provider: {provider_name}"),
                ));
            }
        };

        let hc = match &pdef.healthcheck {
            Some(h) => h,
            None => {
                return Ok(healthcheck_result(
                    false,
                    &format!("No healthcheck configured for {provider_name}"),
                ));
            }
        };

        // Build URL
        let url = if let Some(absolute_url) = &hc.url {
            absolute_url.clone()
        } else {
            let base = llm_config::resolve_base_url(pdef);
            let path = hc.path.as_deref().unwrap_or("");
            format!("{base}{path}")
        };

        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let mut req = match hc.method.to_uppercase().as_str() {
            "POST" => {
                let mut r = client.post(&url).header("Content-Type", "application/json");
                if let Some(body) = &hc.body {
                    r = r.body(body.clone());
                }
                r
            }
            _ => client.get(&url),
        };

        // Apply auth
        req = apply_auth_headers(req, &api_key, Some(pdef));
        if let Some(p) = llm_config::provider_config(&provider_name) {
            for (k, v) in &p.extra_headers {
                req = req.header(k.as_str(), v.as_str());
            }
        }

        match req.send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                let valid = response.status().is_success();
                let body_text = response.text().await.unwrap_or_default();
                let message = if valid {
                    format!("{provider_name} is reachable (HTTP {status})")
                } else {
                    format!("{provider_name} returned HTTP {status}: {body_text}")
                };
                let mut dict = BTreeMap::new();
                dict.insert("valid".to_string(), VmValue::Bool(valid));
                dict.insert("message".to_string(), VmValue::String(Rc::from(message)));
                let mut meta = BTreeMap::new();
                meta.insert("status".to_string(), VmValue::Int(status as i64));
                meta.insert("url".to_string(), VmValue::String(Rc::from(url)));
                dict.insert("metadata".to_string(), VmValue::Dict(Rc::new(meta)));
                Ok(VmValue::Dict(Rc::new(dict)))
            }
            Err(e) => Ok(healthcheck_result(
                false,
                &format!("{provider_name} healthcheck failed: {e}"),
            )),
        }
    });
}

/// Convert a ProviderDef to a VmValue dict for the llm_config builtin.
fn provider_def_to_vm_value(pdef: &llm_config::ProviderDef) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert(
        "base_url".to_string(),
        VmValue::String(Rc::from(pdef.base_url.as_str())),
    );
    dict.insert(
        "auth_style".to_string(),
        VmValue::String(Rc::from(pdef.auth_style.as_str())),
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
    VmValue::Dict(Rc::new(dict))
}

/// Build a healthcheck result dict.
fn healthcheck_result(valid: bool, message: &str) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("valid".to_string(), VmValue::Bool(valid));
    dict.insert("message".to_string(), VmValue::String(Rc::from(message)));
    dict.insert(
        "metadata".to_string(),
        VmValue::Dict(Rc::new(BTreeMap::new())),
    );
    VmValue::Dict(Rc::new(dict))
}

//! Provider-shaped config rows rendered as `VmValue`: a provider definition,
//! its aliases, its credential status, and the whole provider catalog.

use crate::llm_config;
use crate::stdlib::json_to_vm_value;
use crate::value::{VmDictExt, VmValue};

use super::batch_projection::string_list_to_vm_value;
use super::model_projection::model_def_to_vm_value;

/// Region-introspection block for multi-region providers, so a
/// region-aware `.harn` gateway can discover which knobs to set and what
/// is currently resolved without hard-coding provider internals.
///
/// Shape (additive — only present for region-capable providers):
/// `region: { override_key: "region", envs: [<env var names>],
/// resolved: <region|nil> }`. `override_key` names the chain-link field
/// a `routing_policy({...})` route sets to pin a region; `envs` lists the
/// environment variables consulted (in precedence order) when no
/// override is given; `resolved` is the region the env/profile currently
/// yields, or `nil` when nothing is configured yet.
fn provider_region_value(provider_name: Option<&str>) -> Option<VmValue> {
    let name = provider_name?;
    let envs: &[&str] = match name {
        "bedrock" => crate::llm::providers::bedrock::BEDROCK_REGION_ENV_VARS,
        _ => return None,
    };
    let resolved = match name {
        "bedrock" => crate::llm::providers::bedrock::current_env_region(),
        _ => None,
    };
    let mut region = crate::value::DictMap::new();
    region.put_str("override_key", "region");
    region.insert(
        crate::value::intern_key("envs"),
        string_list_to_vm_value(envs.iter().map(|s| s.to_string()).collect()),
    );
    region.insert(
        crate::value::intern_key("resolved"),
        match resolved {
            Some(value) => VmValue::String(arcstr::ArcStr::from(value)),
            None => VmValue::Nil,
        },
    );
    Some(VmValue::dict(region))
}

/// Convert a ProviderDef to a VmValue dict for the llm_config builtin.
pub(super) fn provider_def_to_vm_value(
    provider_name: Option<&str>,
    pdef: &llm_config::ProviderDef,
) -> VmValue {
    let mut dict = crate::value::DictMap::new();
    dict.put_opt_str("display_name", pdef.display_name.as_deref());
    dict.put_opt_str("icon", pdef.icon.as_deref());
    dict.put_opt_str("protocol", pdef.protocol.as_deref());
    dict.put_str("base_url", pdef.base_url.as_str());
    dict.put_opt_str("base_url_env", pdef.base_url_env.as_deref());
    dict.put_str("auth_style", pdef.auth_style.as_str());
    dict.insert(
        crate::value::intern_key("auth_envs"),
        string_list_to_vm_value(llm_config::auth_env_names(&pdef.auth_env)),
    );
    if let Some(region) = provider_region_value(provider_name) {
        dict.insert(crate::value::intern_key("region"), region);
    }
    dict.insert(
        crate::value::intern_key("auth_available"),
        VmValue::Bool(
            provider_name
                .map(|name| crate::llm::provider_auth_status(name).available)
                .unwrap_or(pdef.auth_style == "none"),
        ),
    );
    dict.put_str("chat_endpoint", pdef.chat_endpoint.as_str());
    dict.put_opt_str("completion_endpoint", pdef.completion_endpoint.as_deref());
    dict.put_opt_str("command", pdef.command.as_deref());
    if !pdef.args.is_empty() {
        dict.insert(
            crate::value::intern_key("args"),
            string_list_to_vm_value(pdef.args.clone()),
        );
    }
    if !pdef.env.is_empty() {
        dict.insert(
            crate::value::intern_key("env"),
            json_to_vm_value(&serde_json::json!(pdef.env)),
        );
    }
    dict.put_opt_str("cwd", pdef.cwd.as_deref());
    if !pdef.mcp_servers.is_empty() {
        dict.insert(
            crate::value::intern_key("mcp_servers"),
            json_to_vm_value(&serde_json::Value::Array(pdef.mcp_servers.clone())),
        );
    }
    dict.put_opt_str("auth_header", pdef.auth_header.as_deref());
    if !pdef.extra_headers.is_empty() {
        let mut headers = crate::value::DictMap::new();
        for (k, v) in &pdef.extra_headers {
            headers.insert(
                crate::value::intern_key(k),
                VmValue::String(arcstr::ArcStr::from(v.as_str())),
            );
        }
        dict.insert(
            crate::value::intern_key("extra_headers"),
            VmValue::dict(headers),
        );
    }
    if !pdef.features.is_empty() {
        let features: Vec<VmValue> = pdef
            .features
            .iter()
            .map(|f| VmValue::String(arcstr::ArcStr::from(f.as_str())))
            .collect();
        dict.insert(
            crate::value::intern_key("features"),
            VmValue::List(std::sync::Arc::new(features)),
        );
    }
    if let Some(rpm) = pdef.rpm {
        dict.insert(crate::value::intern_key("rpm"), VmValue::Int(rpm as i64));
    }
    let rate_limits = pdef
        .rate_limits
        .clone()
        .unwrap_or_default()
        .with_rpm_fallback(pdef.rpm);
    if let Some(rate_limits) = rate_limits {
        dict.insert(
            crate::value::intern_key("rate_limits"),
            rate_limits_to_vm_value(&rate_limits),
        );
    }
    if let Some(cost) = pdef.cost_per_1k_in {
        dict.insert(
            crate::value::intern_key("cost_per_1k_in"),
            VmValue::Float(cost),
        );
    }
    if let Some(cost) = pdef.cost_per_1k_out {
        dict.insert(
            crate::value::intern_key("cost_per_1k_out"),
            VmValue::Float(cost),
        );
    }
    if let Some(latency) = pdef.latency_p50_ms {
        dict.insert(
            crate::value::intern_key("latency_p50_ms"),
            VmValue::Int(latency as i64),
        );
    }
    if let Some(performance) = pdef
        .performance
        .as_ref()
        .filter(|performance| !performance.is_empty())
    {
        dict.insert(
            crate::value::intern_key("performance"),
            performance_to_vm_value(performance),
        );
    }
    VmValue::dict(dict)
}

pub(super) fn rate_limits_to_vm_value(rate_limits: &llm_config::RateLimitsDef) -> VmValue {
    json_to_vm_value(&serde_json::to_value(rate_limits).unwrap_or_else(|_| serde_json::json!({})))
}

pub(super) fn performance_to_vm_value(performance: &llm_config::ServingPerformanceDef) -> VmValue {
    json_to_vm_value(&serde_json::to_value(performance).unwrap_or_else(|_| serde_json::json!({})))
}

pub(super) fn provider_catalog_to_vm_value() -> VmValue {
    let mut dict = crate::value::DictMap::new();
    let artifact = crate::provider_catalog::artifact();

    let mut providers = Vec::new();
    for name in llm_config::provider_names() {
        if let Some(pdef) = llm_config::provider_config(&name) {
            let mut provider = match provider_def_to_vm_value(Some(&name), &pdef) {
                VmValue::Dict(provider) => provider.as_ref().clone(),
                _ => unreachable!("provider_def_to_vm_value returns a dict"),
            };
            provider.put_str("name", name.clone());
            providers.push(VmValue::dict(provider));
        }
    }
    dict.insert(
        crate::value::intern_key("providers"),
        VmValue::List(std::sync::Arc::new(providers)),
    );
    dict.insert(
        crate::value::intern_key("known_model_names"),
        string_list_to_vm_value(llm_config::known_model_names()),
    );
    dict.insert(
        crate::value::intern_key("available_providers"),
        string_list_to_vm_value(crate::llm::available_provider_names()),
    );
    let aliases = llm_config::alias_entries()
        .into_iter()
        .map(|(name, alias)| alias_def_to_vm_value(&name, &alias))
        .collect();
    dict.insert(
        crate::value::intern_key("aliases"),
        VmValue::List(std::sync::Arc::new(aliases)),
    );
    let models = llm_config::model_catalog_entries()
        .into_iter()
        .map(|(id, model)| model_def_to_vm_value(&id, &model))
        .collect();
    dict.insert(
        crate::value::intern_key("models"),
        VmValue::List(std::sync::Arc::new(models)),
    );
    dict.insert(
        crate::value::intern_key("variants"),
        json_to_vm_value(
            &serde_json::to_value(&artifact.variants).unwrap_or_else(|_| serde_json::json!([])),
        ),
    );
    dict.insert(
        crate::value::intern_key("families"),
        json_to_vm_value(
            &serde_json::to_value(&artifact.families).unwrap_or_else(|_| serde_json::json!([])),
        ),
    );
    dict.insert(
        crate::value::intern_key("routing_routes"),
        json_to_vm_value(
            &serde_json::to_value(&artifact.routing_routes)
                .unwrap_or_else(|_| serde_json::json!([])),
        ),
    );
    let qc_defaults = llm_config::qc_defaults()
        .into_iter()
        .map(|(provider, model)| {
            (
                crate::value::intern_key(&provider),
                VmValue::String(arcstr::ArcStr::from(model)),
            )
        })
        .collect::<crate::value::DictMap>();
    dict.insert(
        crate::value::intern_key("qc_defaults"),
        VmValue::dict(qc_defaults),
    );

    VmValue::dict(dict)
}

fn alias_def_to_vm_value(name: &str, alias: &llm_config::AliasDef) -> VmValue {
    let mut dict = crate::value::DictMap::new();
    dict.put_str("name", name);
    dict.put_str("id", alias.id.as_str());
    dict.put_str("provider", alias.provider.as_str());
    dict.insert(
        crate::value::intern_key("tool_format"),
        alias
            .tool_format
            .as_deref()
            .map(|format| VmValue::String(arcstr::ArcStr::from(format)))
            .unwrap_or(VmValue::Nil),
    );
    VmValue::dict(dict)
}

pub(crate) fn llm_provider_status_value() -> VmValue {
    let statuses = crate::llm::provider_auth_statuses();
    let mut entries = Vec::with_capacity(statuses.len());
    for status in statuses {
        let mut entry = crate::value::DictMap::new();
        entry.put_str("name", status.name);
        entry.insert(
            crate::value::intern_key("available"),
            VmValue::Bool(status.available),
        );
        entry.put_str("credential_status", status.credential_status.as_str());
        entries.push(VmValue::dict(entry));
    }
    VmValue::List(std::sync::Arc::new(entries))
}

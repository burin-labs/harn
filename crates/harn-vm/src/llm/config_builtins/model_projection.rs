//! Model-shaped config rows rendered as `VmValue`: a resolved selector, a
//! catalog row, and the full catalog list.

use crate::llm_config;
use crate::stdlib::json_to_vm_value;
use crate::value::{VmDictExt, VmValue};

use super::batch_projection::string_list_to_vm_value;
use super::capability_projection::{capabilities_to_vm_value, insert_batch_support_fields};
use super::catalog_projection::insert_tool_mode_parity_fields;
use super::provider_projection::{performance_to_vm_value, rate_limits_to_vm_value};

pub(crate) fn llm_catalog_value() -> VmValue {
    let entries: Vec<VmValue> = llm_config::model_catalog_entries()
        .into_iter()
        .map(|(id, model)| model_def_to_vm_value(&id, &model))
        .collect();
    VmValue::List(std::sync::Arc::new(entries))
}

pub(super) fn resolved_model_to_vm_value(resolved: &llm_config::ResolvedModel) -> VmValue {
    let mut dict = crate::value::DictMap::new();
    dict.put_str("id", resolved.id.as_str());
    dict.put_str("provider", resolved.provider.as_str());
    dict.insert(
        crate::value::intern_key("alias"),
        resolved
            .alias
            .as_deref()
            .map(|alias| VmValue::String(arcstr::ArcStr::from(alias)))
            .unwrap_or(VmValue::Nil),
    );
    dict.put_str("tool_format", resolved.tool_format.as_str());
    dict.put_str("tier", resolved.tier.as_str());
    dict.put_str("family", resolved.family.as_str());
    dict.put_str("lineage", resolved.lineage.as_str());
    VmValue::dict(dict)
}

pub(super) fn model_info_to_vm_value(resolved: &llm_config::ResolvedModel) -> VmValue {
    let mut dict = match resolved_model_to_vm_value(resolved) {
        VmValue::Dict(dict) => dict.as_ref().clone(),
        _ => unreachable!("resolved_model_to_vm_value returns a dict"),
    };
    let caps = crate::llm::capabilities::lookup(&resolved.provider, &resolved.id);
    dict.insert(
        crate::value::intern_key("capabilities"),
        capabilities_to_vm_value(&resolved.provider, &resolved.id, &caps),
    );
    dict.insert(
        crate::value::intern_key("catalog"),
        llm_config::model_catalog_entry(&resolved.id)
            .map(|entry| model_def_to_vm_value(&resolved.id, &entry))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("qc_default_model"),
        llm_config::qc_default_model(&resolved.provider)
            .map(|model| VmValue::String(arcstr::ArcStr::from(model)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("auth_available"),
        VmValue::Bool(crate::llm::provider_auth_status(&resolved.provider).available),
    );
    VmValue::dict(dict)
}

pub(super) fn model_def_to_vm_value(id: &str, model: &llm_config::ModelDef) -> VmValue {
    let mut dict = crate::value::DictMap::new();
    let capabilities = crate::llm::capabilities::lookup(&model.provider, id);
    let batch_api =
        crate::llm_config::effective_batch_api_supported(&model.provider, &capabilities);
    dict.put_str("id", id);
    dict.put_str("name", model.name.as_str());
    dict.insert(
        crate::value::intern_key("blurb"),
        model
            .blurb
            .as_deref()
            .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
            .unwrap_or(VmValue::Nil),
    );
    dict.put_str("provider", model.provider.as_str());
    dict.insert(
        crate::value::intern_key("context_window"),
        VmValue::Int(model.context_window as i64),
    );
    dict.insert(
        crate::value::intern_key("logical_model"),
        model
            .logical_model
            .as_deref()
            .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("equivalence_group"),
        model
            .equivalence_group
            .as_deref()
            .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("served_variant"),
        model
            .served_variant
            .as_deref()
            .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("wire_model"),
        model
            .wire_model
            .as_deref()
            .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("api_dialect"),
        model
            .api_dialect
            .as_deref()
            .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("rate_limits"),
        model
            .rate_limits
            .as_ref()
            .filter(|limits| !limits.is_empty())
            .map(rate_limits_to_vm_value)
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("performance"),
        model
            .performance
            .as_ref()
            .filter(|performance| !performance.is_empty())
            .map(performance_to_vm_value)
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("architecture"),
        model
            .architecture
            .as_ref()
            .filter(|architecture| !architecture.is_empty())
            .map(architecture_to_vm_value)
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("runtime_context_window"),
        model
            .runtime_context_window
            .map(|window| VmValue::Int(window as i64))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("stream_timeout"),
        model
            .stream_timeout
            .map(VmValue::Float)
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("capabilities"),
        string_list_to_vm_value(model.capabilities.clone()),
    );
    insert_tool_mode_parity_fields(&mut dict, &capabilities);
    dict.insert(
        crate::value::intern_key("reasoning_effort_levels"),
        string_list_to_vm_value(capabilities.reasoning_effort_levels.clone()),
    );
    insert_batch_support_fields(&mut dict, batch_api, &capabilities);
    dict.insert(
        crate::value::intern_key("pricing"),
        model
            .pricing
            .as_ref()
            .map(pricing_to_vm_value)
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("deprecated"),
        VmValue::Bool(model.deprecated),
    );
    dict.insert(
        crate::value::intern_key("deprecation_note"),
        model
            .deprecation_note
            .as_deref()
            .map(|note| VmValue::String(arcstr::ArcStr::from(note)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("superseded_by"),
        model
            .superseded_by
            .as_deref()
            .map(|target| VmValue::String(arcstr::ArcStr::from(target)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("quality_tags"),
        string_list_to_vm_value(model.quality_tags.clone()),
    );
    dict.put_str("family", llm_config::model_family(&model.provider, id));
    dict.put_str("lineage", llm_config::model_lineage(&model.provider, id));
    dict.insert(
        crate::value::intern_key("complementary_with"),
        string_list_to_vm_value(model.complementary_with.clone()),
    );
    dict.insert(
        crate::value::intern_key("avoid_as_reviewer_for"),
        string_list_to_vm_value(model.avoid_as_reviewer_for.clone()),
    );
    dict.put_str("availability", model.availability.as_str());
    dict.put_str("tier", llm_config::model_tier(id));
    dict.insert(
        crate::value::intern_key("open_weight"),
        model.open_weight.map(VmValue::Bool).unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("strengths"),
        string_list_to_vm_value(model.strengths.clone()),
    );
    let benchmarks: crate::value::DictMap = model
        .benchmarks
        .iter()
        .map(|(key, value)| (crate::value::intern_key(key), VmValue::Float(*value)))
        .collect();
    dict.insert(
        crate::value::intern_key("benchmarks"),
        VmValue::dict(benchmarks),
    );
    dict.insert(
        crate::value::intern_key("serving_tiers"),
        json_to_vm_value(
            &serde_json::to_value(&model.serving_tiers).unwrap_or_else(|_| serde_json::json!([])),
        ),
    );
    VmValue::dict(dict)
}

fn architecture_to_vm_value(architecture: &llm_config::ModelArchitectureDef) -> VmValue {
    json_to_vm_value(&serde_json::to_value(architecture).unwrap_or_else(|_| serde_json::json!({})))
}

fn pricing_to_vm_value(pricing: &llm_config::ModelPricing) -> VmValue {
    let mut dict = crate::value::DictMap::new();
    dict.insert(
        crate::value::intern_key("input_per_mtok"),
        VmValue::Float(pricing.input_per_mtok),
    );
    dict.insert(
        crate::value::intern_key("output_per_mtok"),
        VmValue::Float(pricing.output_per_mtok),
    );
    dict.insert(
        crate::value::intern_key("cache_read_per_mtok"),
        pricing
            .cache_read_per_mtok
            .map(VmValue::Float)
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        crate::value::intern_key("cache_write_per_mtok"),
        pricing
            .cache_write_per_mtok
            .map(VmValue::Float)
            .unwrap_or(VmValue::Nil),
    );
    VmValue::dict(dict)
}

//! Resolved identities and catalog rows with their script-facing behavior overrides.

use crate::llm_config;
use crate::value::{VmDictExt, VmValue};

use super::capability_projection::{capabilities_to_vm_value, insert_batch_support_fields};
use super::catalog_projection::insert_tool_mode_parity_fields;
use super::serialization::{fields, value};

#[derive(Clone, Copy)]
pub(super) enum PricingView {
    Effective,
    Stored,
}

pub(crate) fn llm_catalog_value() -> VmValue {
    VmValue::list(
        llm_config::model_catalog_entries()
            .into_iter()
            .map(|(id, model)| model_def_to_vm_value(&id, &model, PricingView::Effective))
            .collect(),
    )
}

pub(super) fn resolved_model_to_vm_value(resolved: &llm_config::ResolvedModel) -> VmValue {
    value(resolved)
}

pub(super) fn model_info_to_vm_value(resolved: &llm_config::ResolvedModel) -> VmValue {
    let mut dict = resolved_model_to_vm_value(resolved)
        .as_dict()
        .expect("resolved model record")
        .as_ref()
        .clone();
    let caps = crate::llm::capabilities::lookup(&resolved.provider, &resolved.id);
    dict.put(
        "capabilities",
        capabilities_to_vm_value(&resolved.provider, &resolved.id, &caps),
    );
    dict.put_nullable(
        "catalog",
        llm_config::model_catalog_entry(&resolved.id)
            .map(|entry| model_def_to_vm_value(&resolved.id, &entry, PricingView::Effective)),
    );
    dict.put_nullable_str(
        "qc_default_model",
        llm_config::qc_default_model(&resolved.provider),
    );
    dict.put_bool(
        "auth_available",
        crate::llm::provider_auth_status(&resolved.provider).available,
    );
    VmValue::dict(dict)
}

pub(super) fn model_def_to_vm_value(
    id: &str,
    model: &llm_config::ModelDef,
    pricing_view: PricingView,
) -> VmValue {
    let mut dict = fields!(model;
        name,
        blurb,
        provider,
        operations,
        logical_model,
        equivalence_group,
        served_variant,
        wire_model,
        api_dialect,
        capabilities,
        deprecated,
        deprecation_note,
        sunset_date,
        retires_on,
        superseded_by,
        quality_tags,
        complementary_with,
        avoid_as_reviewer_for,
        availability,
        open_weight,
        strengths,
        serving_tiers,
    );
    dict.put_str("id", id);
    dict.put_int("context_window", model.context_window as i64);
    dict.put_nullable(
        "runtime_context_window",
        model
            .runtime_context_window
            .map(|window| VmValue::Int(window as i64)),
    );
    dict.put_nullable("stream_timeout", model.stream_timeout.map(VmValue::Float));
    dict.put(
        "rate_limits",
        value(
            &model
                .rate_limits
                .as_ref()
                .filter(|limits| !limits.is_empty()),
        ),
    );
    dict.put(
        "performance",
        value(
            &model
                .performance
                .as_ref()
                .filter(|performance| !performance.is_empty()),
        ),
    );
    dict.put(
        "architecture",
        value(
            &model
                .architecture
                .as_ref()
                .filter(|architecture| !architecture.is_empty()),
        ),
    );
    let caps = crate::llm::capabilities::lookup(&model.provider, id);
    insert_tool_mode_parity_fields(&mut dict, &caps);
    dict.put(
        "reasoning_effort_levels",
        value(&caps.reasoning_effort_levels),
    );
    insert_batch_support_fields(
        &mut dict,
        llm_config::effective_batch_api_supported(&model.provider, &caps),
        &caps,
    );
    dict.put_nullable(
        "pricing",
        model
            .pricing
            .as_ref()
            .map(|pricing| pricing_to_vm_value(pricing, pricing_view)),
    );
    dict.put_str("family", llm_config::model_family(&model.provider, id));
    dict.put_str("lineage", llm_config::model_lineage(&model.provider, id));
    dict.put_str("tier", llm_config::model_tier(id));
    dict.put_nullable(
        "completion_review",
        model
            .completion_review
            .as_ref()
            .map(|review| VmValue::dict(fields!(review; scrutiny, max_judge_calls, evidence))),
    );
    dict.put(
        "benchmarks",
        VmValue::dict(
            model
                .benchmarks
                .iter()
                .map(|(key, value)| (crate::value::intern_key(key), VmValue::Float(*value))),
        ),
    );
    VmValue::dict(dict)
}

fn pricing_to_vm_value(pricing: &llm_config::ModelPricing, view: PricingView) -> VmValue {
    if matches!(view, PricingView::Stored) {
        return value(pricing);
    }
    let pricing = pricing.effective_today();
    value(&serde_json::json!({
        "input_per_mtok": pricing.input_per_mtok,
        "output_per_mtok": pricing.output_per_mtok,
        "cache_read_per_mtok": pricing.cache_read_per_mtok,
        "cache_write_per_mtok": pricing.cache_write_per_mtok,
    }))
}

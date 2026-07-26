//! Which provider and model a selector resolves to, and the options a call
//! against that route inherits.

use crate::llm_config;
use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmDictExt, VmError, VmValue};

use super::batch_projection::string_list_to_vm_value;
use super::catalog_projection::toml_value_to_vm_value;
use super::model_projection::{
    model_def_to_vm_value, model_info_to_vm_value, resolved_model_to_vm_value,
};

/// Infer the configured provider name for a model identifier.
#[harn_builtin(
    sig = "llm_infer_provider(model_id: string) -> string",
    category = "llm.config"
)]
fn llm_infer_provider_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let model_id = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(arcstr::ArcStr::from(
        llm_config::infer_provider(&model_id),
    )))
}

/// Return the configured capability tier for a model identifier.
#[harn_builtin(
    sig = "llm_model_tier(model_id: string) -> string",
    category = "llm.config"
)]
fn llm_model_tier_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let model_id = args.first().map(|a| a.display()).unwrap_or_default();
    Ok(VmValue::String(arcstr::ArcStr::from(
        llm_config::model_tier(&model_id),
    )))
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
        .map(|model| VmValue::String(arcstr::ArcStr::from(model)))
        .unwrap_or(VmValue::Nil))
}

/// Return logical- and route-merged model defaults for `model_id`.
#[harn_builtin(
    sig = "llm_model_defaults(model_id: string) -> dict",
    category = "llm.config"
)]
pub(super) fn llm_model_defaults_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let model_id = args.first().map(|a| a.display()).unwrap_or_default();
    if model_id.is_empty() {
        return Err(VmError::Runtime(
            "llm_model_defaults: model_id is required".to_string(),
        ));
    }
    let resolved = llm_config::resolve_model_info(&model_id);
    let params = llm_config::model_params_for_route(&resolved.provider, &resolved.id);
    let mut dict = crate::value::DictMap::new();
    for (k, v) in &params {
        dict.insert(crate::value::intern_key(k), toml_value_to_vm_value(v));
    }
    Ok(VmValue::dict(dict))
}

/// Return the fully-merged llm_call options for `opts`. Requires opts.model.
#[harn_builtin(
    sig = "llm_resolved_options(opts: dict) -> dict",
    category = "llm.config"
)]
pub(super) fn llm_resolved_options_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
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
    let defaults = llm_config::model_params_for_route(&final_provider, &resolved_id);
    let mut out = opts.clone();
    for (k, v) in &defaults {
        if !out.contains_key(k.as_str()) {
            out.insert(crate::value::intern_key(k), toml_value_to_vm_value(v));
        }
    }
    out.put_str("provider", final_provider);
    out.put_str("model", resolved_id);
    Ok(VmValue::dict(out))
}

/// Apply Harn's provider-aware reasoning-policy and effort defaults to an `llm_call` option dict.
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
    let out = crate::llm::reasoning_policy::apply_policy_to_vm_options(opts)?;
    Ok(VmValue::dict(out))
}

/// Resolve the canonical reasoning-channel output budget (tokens) for an effort
/// level, mirroring the policy used when materializing a `thinking` budget. This
/// is the single source of truth for the effort -> token mapping so callers
/// (e.g. the `std/llm/safe` structured-floor fallback) do not re-hardcode it.
/// An unknown/empty level resolves to the medium default.
#[harn_builtin(
    sig = "llm_reasoning_effort_budget(level: string) -> int",
    category = "llm.config"
)]
pub(super) fn llm_reasoning_effort_budget_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let level = args.first().map(|a| a.display()).unwrap_or_default();
    let budget = crate::llm::reasoning_policy::budget_for_reasoning_level(level.trim());
    Ok(VmValue::Int(i64::from(budget)))
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

    let mut dict = crate::value::DictMap::new();
    dict.put_str("id", id.clone());
    dict.put_str("provider", provider);
    dict.put_str("tier", llm_config::model_tier(&id));
    Ok(VmValue::dict(dict))
}

/// Pick a different-family reviewer model for review, critique, or plan review.
#[harn_builtin(
    sig = "llm_complementary_reviewer(options: dict) -> dict",
    category = "llm.config"
)]
fn llm_complementary_reviewer_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let options = parse_complementary_reviewer_options(args.first())?;
    let selection = llm_config::pick_complementary_reviewer(options);
    let json = serde_json::to_value(selection).map_err(|error| {
        VmError::Runtime(format!(
            "llm_complementary_reviewer: serialize result: {error}"
        ))
    })?;
    Ok(json_to_vm_value(&json))
}

fn parse_complementary_reviewer_options(
    value: Option<&VmValue>,
) -> Result<llm_config::ComplementaryReviewerOptions, VmError> {
    let dict = value.and_then(|value| value.as_dict()).ok_or_else(|| {
        VmError::Runtime("llm_complementary_reviewer: options must be a dict".to_string())
    })?;
    let author_model = dict
        .get("author_model")
        .or_else(|| dict.get("model"))
        .map(|value| value.display())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            VmError::Runtime(
                "llm_complementary_reviewer: options.author_model is required".to_string(),
            )
        })?;
    let author_provider = dict
        .get("author_provider")
        .or_else(|| dict.get("provider"))
        .map(|value| value.display())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let intent = dict
        .get("intent")
        .map(|value| value.display())
        .unwrap_or_else(|| "review".to_string());
    let intent =
        llm_config::ComplementaryReviewerIntent::parse(intent.trim()).ok_or_else(|| {
            VmError::Runtime(
                "llm_complementary_reviewer: intent must be review, critique, or plan_review"
                    .to_string(),
            )
        })?;
    let max_price_multiplier = dict
        .get("max_price_multiplier")
        .map(vm_value_as_f64)
        .transpose()?;
    if max_price_multiplier.is_some_and(|value| !value.is_finite() || value <= 0.0) {
        return Err(VmError::Runtime(
            "llm_complementary_reviewer: max_price_multiplier must be positive".to_string(),
        ));
    }
    Ok(llm_config::ComplementaryReviewerOptions {
        author_model,
        author_provider,
        intent,
        max_price_multiplier,
    })
}

fn vm_value_as_f64(value: &VmValue) -> Result<f64, VmError> {
    match value {
        VmValue::Float(value) => Ok(*value),
        VmValue::Int(value) => Ok(*value as f64),
        other => Err(VmError::Runtime(format!(
            "llm_complementary_reviewer: max_price_multiplier must be numeric, got {}",
            other.type_name()
        ))),
    }
}

/// Return same-logical-model routes suitable for explicit failover or
/// experiment comparison. The result intentionally excludes the selected
/// source route and only includes non-deprecated, non-dedicated catalog rows
/// with compatible context/tool/structured-output capabilities.
#[harn_builtin(
    sig = "llm_equivalent_models(selector: string) -> list",
    category = "llm.config"
)]
fn llm_equivalent_models_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let selector = args
        .first()
        .map(|value| value.display())
        .unwrap_or_default();
    if selector.trim().is_empty() {
        return Err(VmError::Runtime(
            "llm_equivalent_models: selector is required".to_string(),
        ));
    }
    let entries = llm_config::equivalent_model_catalog_entries(selector.trim())
        .into_iter()
        .map(|(id, model)| model_def_to_vm_value(&id, &model))
        .collect();
    Ok(VmValue::List(std::sync::Arc::new(entries)))
}

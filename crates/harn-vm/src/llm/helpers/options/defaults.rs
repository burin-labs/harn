use super::*;

/// Layer the active step's defaults onto the call options dict before
/// model/provider resolution and budget parsing run. The model override
/// is a no-op when the user explicitly passed `model:`. The budget
/// merge is non-destructive: only fields the call site didn't already
/// set are filled in, so a tighter explicit ceiling always wins.
pub(super) fn apply_active_step_defaults(options: &mut Option<BTreeMap<String, VmValue>>) {
    let user_supplied_model = options
        .as_ref()
        .map(|o| o.contains_key("model"))
        .unwrap_or(false);
    let step_default = if user_supplied_model {
        None
    } else {
        crate::step_runtime::active_step_model_default()
    };
    let step_budget = crate::step_runtime::with_active_step(|step| step.definition.clone())
        .map(|definition| (definition.max_tokens, definition.max_usd));
    if step_default.is_none() && step_budget.is_none() {
        return;
    }
    let opts = options.get_or_insert_with(BTreeMap::new);
    if let Some(model_name) = step_default {
        opts.insert(
            "model".to_string(),
            VmValue::String(std::sync::Arc::from(model_name)),
        );
    }
    if let Some((max_tokens, max_usd)) = step_budget {
        if max_tokens.is_some() || max_usd.is_some() {
            // Project the step budget onto `llm_call`'s preflight
            // budget envelope so the existing accumulator + projection
            // machinery short-circuits a call that would obviously
            // exceed the step's ceiling.
            let mut step_budget_dict: BTreeMap<String, VmValue> = match opts.get("budget") {
                Some(VmValue::Dict(existing)) => (**existing).clone(),
                _ => BTreeMap::new(),
            };
            if let Some(max_tokens) = max_tokens {
                step_budget_dict
                    .entry("max_output_tokens".to_string())
                    .or_insert_with(|| VmValue::Int(max_tokens as i64));
            }
            if let Some(max_usd) = max_usd {
                step_budget_dict
                    .entry("max_cost_usd".to_string())
                    .or_insert_with(|| VmValue::Float(max_usd));
            }
            opts.insert(
                "budget".to_string(),
                VmValue::Dict(std::sync::Arc::new(step_budget_dict)),
            );
        }
    }
}

pub(super) fn toml_value_to_vm_value(value: &toml::Value) -> VmValue {
    match value {
        toml::Value::String(s) => VmValue::String(std::sync::Arc::from(s.as_str())),
        toml::Value::Integer(i) => VmValue::Int(*i),
        toml::Value::Float(f) => VmValue::Float(*f),
        toml::Value::Boolean(b) => VmValue::Bool(*b),
        toml::Value::Datetime(dt) => VmValue::String(std::sync::Arc::from(dt.to_string())),
        toml::Value::Array(items) => VmValue::List(std::sync::Arc::new(
            items.iter().map(toml_value_to_vm_value).collect(),
        )),
        toml::Value::Table(table) => VmValue::Dict(std::sync::Arc::new(
            table
                .iter()
                .map(|(key, value)| (key.clone(), toml_value_to_vm_value(value)))
                .collect(),
        )),
    }
}

pub(super) fn model_role_option(options: &Option<BTreeMap<String, VmValue>>) -> Option<String> {
    options
        .as_ref()
        .and_then(|opts| opts.get("model_role").or_else(|| opts.get("role")))
        .filter(|value| !matches!(value, VmValue::Nil | VmValue::Bool(false)))
        .map(VmValue::display)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn apply_model_role_defaults(options: &mut Option<BTreeMap<String, VmValue>>) {
    let Some(role) = model_role_option(options) else {
        return;
    };
    let defaults = crate::llm_config::model_role_defaults(&role);
    if defaults.is_empty() {
        return;
    }
    let opts = options.get_or_insert_with(BTreeMap::new);
    for (key, value) in defaults {
        if key == "model_role" || key == "role" {
            continue;
        }
        opts.entry(key)
            .or_insert_with(|| toml_value_to_vm_value(&value));
    }
}

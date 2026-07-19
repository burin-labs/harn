//! The durable, secret-free projection of an effective model route.

use crate::llm_config;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmDictExt, VmError, VmValue};

/// Return the Harn-resolved model-route facts that are safe to persist in
/// replay, eval, and audit receipts.
#[harn_builtin(
    sig = "llm_execution_contract(selector: string) -> dict",
    category = "llm.config"
)]
fn llm_execution_contract_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let selector = args.first().map(|a| a.display()).unwrap_or_default();
    let selector = selector.trim();
    if selector.is_empty() {
        return Err(VmError::Runtime(
            "llm_execution_contract: selector is required".to_string(),
        ));
    }
    Ok(model_execution_contract_to_vm_value(
        &llm_config::model_execution_contract(selector),
    ))
}

fn model_execution_contract_to_vm_value(contract: &llm_config::ModelExecutionContract) -> VmValue {
    let mut dict = crate::value::DictMap::new();
    dict.put_str("schema", "harn.llm.execution-contract/v1");
    dict.put_str("selector", contract.selector.as_str());
    dict.put_str("model_id", contract.resolved.id.as_str());
    dict.put_str("provider", contract.resolved.provider.as_str());
    dict.put_str("wire_model", contract.wire_model.as_str());
    dict.put_str("tool_format", contract.resolved.tool_format.as_str());
    dict.put_str("tier", contract.resolved.tier.as_str());
    dict.put_str("family", contract.resolved.family.as_str());
    dict.put_str("lineage", contract.resolved.lineage.as_str());

    let mut defaults = crate::value::DictMap::new();
    for (key, value) in &contract.generation_defaults {
        defaults.insert(
            crate::value::intern_key(key),
            super::catalog_projection::toml_value_to_vm_value(value),
        );
    }
    dict.insert(
        crate::value::intern_key("generation_defaults"),
        VmValue::dict(defaults),
    );
    VmValue::dict(dict)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::llm_model_defaults_builtin;
    use super::*;

    #[test]
    fn execution_contract_projects_only_valid_generation_defaults() {
        let _guard = crate::llm::env_guard();
        llm_config::clear_user_overrides();

        let mut overlay = llm_config::ProvidersConfig::default();
        overlay.model_defaults.insert(
            "receipt-contract-model".to_string(),
            BTreeMap::from([
                ("temperature".to_string(), toml::Value::Float(0.7)),
                ("max_tokens".to_string(), toml::Value::Integer(512)),
                (
                    "operator_token".to_string(),
                    toml::Value::String("not-for-receipts".to_string()),
                ),
                (
                    "nested_private".to_string(),
                    toml::Value::Table(toml::map::Map::from_iter([(
                        "api_key".to_string(),
                        toml::Value::String("not-for-receipts".to_string()),
                    )])),
                ),
                (
                    "top_p".to_string(),
                    toml::Value::Table(toml::map::Map::from_iter([(
                        "private".to_string(),
                        toml::Value::String("not-a-generation-default".to_string()),
                    )])),
                ),
            ]),
        );
        llm_config::set_user_overrides(Some(overlay));

        let mut out = String::new();
        let raw_defaults = llm_model_defaults_builtin(
            &[VmValue::String(arcstr::ArcStr::from(
                "receipt-contract-model",
            ))],
            &mut out,
        )
        .expect("raw route defaults");
        assert!(
            raw_defaults
                .as_dict()
                .is_some_and(|defaults| defaults.contains_key("operator_token")),
            "receipt filtering must not alter inference defaults",
        );

        let result = llm_execution_contract_builtin(
            &[VmValue::String(arcstr::ArcStr::from(
                "receipt-contract-model",
            ))],
            &mut out,
        )
        .expect("execution contract");
        let contract = result.as_dict().expect("contract dict");

        assert_eq!(
            contract.get("schema").map(VmValue::display).as_deref(),
            Some("harn.llm.execution-contract/v1")
        );
        assert_eq!(
            contract.get("selector").map(VmValue::display).as_deref(),
            Some("receipt-contract-model")
        );
        let defaults = contract
            .get("generation_defaults")
            .and_then(VmValue::as_dict)
            .expect("generation defaults dict");
        assert!(
            matches!(defaults.get("temperature"), Some(VmValue::Float(value)) if *value == 0.7)
        );
        assert!(matches!(
            defaults.get("max_tokens"),
            Some(VmValue::Int(512))
        ));
        assert!(!defaults.contains_key("operator_token"));
        assert!(!defaults.contains_key("nested_private"));
        assert!(!defaults.contains_key("top_p"));

        llm_config::clear_user_overrides();
    }

    #[test]
    fn execution_contract_requires_a_selector() {
        let err = llm_execution_contract_builtin(&[], &mut String::new())
            .expect_err("missing selector must fail loudly");
        assert!(
            matches!(err, VmError::Runtime(message) if message == "llm_execution_contract: selector is required")
        );
    }
}

//! What exists: configured providers, their capability rows, the model
//! catalog, and the runtime overlay that can refresh it.

use crate::llm_config;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

use super::batch_projection::string_list_to_vm_value;
use super::capability_projection::capabilities_to_vm_value;
use super::provider_projection::{provider_catalog_to_vm_value, provider_def_to_vm_value};

/// Return provider/model capability metadata from the loaded capability matrix.
#[harn_builtin(
    exposure = "harness.llm.provider_capabilities",
    effects = ["llm.read@arg0"],
    sig = "provider_capabilities(provider: string, model?: string|nil) -> dict",
    category = "llm.config"
)]
pub(super) fn provider_capabilities_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let provider = args.first().map(|a| a.display()).unwrap_or_default();
    let model = args.get(1).map(|a| a.display()).unwrap_or_default();
    if provider.is_empty() {
        return Err(VmError::Runtime(
            "provider_capabilities: provider name is required".to_string(),
        ));
    }
    let caps = crate::llm::capabilities::lookup(&provider, &model);
    Ok(capabilities_to_vm_value(&provider, &model, &caps))
}

/// Install raw TOML capability overrides for provider/model capability lookup.
#[harn_builtin(
    exposure = "harness.llm.provider_capabilities_install",
    effects = ["state.mutate@arg0"],
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
    crate::llm::capabilities::set_user_overrides_toml(&src).map_err(|e| {
        VmError::Runtime(format!("provider_capabilities_install: parse error: {e}"))
    })?;
    Ok(VmValue::Bool(true))
}

/// Clear installed provider/model capability overrides.
#[harn_builtin(
    exposure = "harness.llm.provider_capabilities_clear",
    effects = ["state.mutate@const=provider-capabilities"],
    sig = "provider_capabilities_clear() -> bool", category = "llm.config"
)]
fn provider_capabilities_clear_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    crate::llm::capabilities::clear_user_overrides();
    Ok(VmValue::Bool(true))
}

/// List providers usable in the current environment.
#[harn_builtin(
    exposure = "harness.llm.available_providers",
    effects = ["llm.read@const=provider-catalog"],
    sig = "llm_available_providers() -> list", category = "llm.config"
)]
fn llm_available_providers_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(string_list_to_vm_value(
        crate::llm::available_provider_names(),
    ))
}

/// Return the loaded provider, alias, model, pricing, and availability catalog.
#[harn_builtin(
    exposure = "harness.llm.provider_catalog",
    effects = ["llm.read@const=provider-catalog"],
    sig = "llm_provider_catalog() -> dict", category = "llm.config"
)]
fn llm_provider_catalog_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(provider_catalog_to_vm_value())
}

/// Register a custom OpenAI-compatible provider name for runtime dispatch.
#[harn_builtin(
    exposure = "harness.llm.provider_register",
    effects = ["state.mutate@arg0"],
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
    crate::llm::provider::register_provider_name(&name);
    Ok(VmValue::Bool(true))
}

pub(crate) fn parse_catalog_refresh_options(
    value: Option<&VmValue>,
    context: &str,
) -> Result<crate::provider_catalog::CatalogRefreshOptions, VmError> {
    let Some(value) = value else {
        return Ok(crate::provider_catalog::CatalogRefreshOptions::default());
    };
    if matches!(value, VmValue::Nil) {
        return Ok(crate::provider_catalog::CatalogRefreshOptions::default());
    }
    let dict = value
        .as_dict()
        .ok_or_else(|| VmError::Runtime(format!("{context}: options must be a dict or nil")))?;
    let url = dict
        .get("url")
        .map(|value| value.display())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let force = dict
        .get("force")
        .is_some_and(|value| matches!(value, VmValue::Bool(true)));
    Ok(crate::provider_catalog::CatalogRefreshOptions { url, force })
}

/// Return configured provider settings, or all provider settings when no provider is passed.
#[harn_builtin(
    exposure = "harness.llm.config",
    effects = ["state.mutate@const=llm-config"],
    sig = "llm_config(provider?: string|nil) -> dict|nil",
    category = "llm.config"
)]
pub(super) fn llm_config_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
            let mut dict = crate::value::DictMap::new();
            for name in llm_config::provider_names() {
                if let Some(pdef) = llm_config::provider_config(&name) {
                    dict.insert(
                        crate::value::intern_key(&name),
                        provider_def_to_vm_value(Some(&name), &pdef),
                    );
                }
            }
            Ok(VmValue::dict(dict))
        }
    }
}

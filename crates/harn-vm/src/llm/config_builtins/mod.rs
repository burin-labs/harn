//! Config-facing LLM builtins: the read-only surface a `.harn` script uses to
//! ask what providers and models exist, how a selector resolves, what a route
//! is capable of, and what operational limits apply.
//!
//! Builtins are grouped by the question they answer — [`catalog_builtins`]
//! (what exists), [`selection_builtins`] (what should I use, with what
//! options), [`rate_limit_builtins`] and [`healthcheck_builtins`] (can I use
//! it right now) — and each shares the `*_projection` modules that render a
//! config row as a `VmValue`.

mod batch_projection;
mod capability_projection;
mod catalog_builtins;
mod catalog_projection;
mod execution_contract;
mod healthcheck_builtins;
mod model_projection;
mod option_registry;
mod provider_projection;
mod rate_limit_builtins;
mod selection_builtins;

#[cfg(test)]
mod tests;

use crate::stdlib::macros::{register_builtin_defs, VmBuiltinDef};
use crate::vm::Vm;

use self::catalog_builtins::{
    LLM_AVAILABLE_PROVIDERS_BUILTIN_DEF, LLM_CATALOG_BUILTIN_DEF, LLM_CATALOG_REFRESH_BUILTIN_DEF,
    LLM_CONFIG_BUILTIN_DEF, LLM_PROVIDERS_BUILTIN_DEF, LLM_PROVIDER_CATALOG_BUILTIN_DEF,
    LLM_PROVIDER_STATUS_BUILTIN_DEF, PROVIDER_CAPABILITIES_BUILTIN_DEF,
    PROVIDER_CAPABILITIES_CLEAR_BUILTIN_DEF, PROVIDER_CAPABILITIES_INSTALL_BUILTIN_DEF,
    PROVIDER_REGISTER_BUILTIN_DEF,
};
use self::execution_contract::LLM_EXECUTION_CONTRACT_BUILTIN_DEF;
use self::healthcheck_builtins::LLM_HEALTHCHECK_BUILTIN_DEF;
use self::rate_limit_builtins::LLM_RATE_LIMIT_BUILTIN_DEF;
use self::selection_builtins::{
    LLM_APPLY_REASONING_POLICY_BUILTIN_DEF, LLM_COMPLEMENTARY_REVIEWER_BUILTIN_DEF,
    LLM_EQUIVALENT_MODELS_BUILTIN_DEF, LLM_INFER_PROVIDER_BUILTIN_DEF,
    LLM_KNOWN_MODELS_BUILTIN_DEF, LLM_MODEL_DEFAULTS_BUILTIN_DEF, LLM_MODEL_INFO_BUILTIN_DEF,
    LLM_MODEL_TIER_BUILTIN_DEF, LLM_PICK_MODEL_BUILTIN_DEF, LLM_QC_DEFAULT_MODEL_BUILTIN_DEF,
    LLM_REASONING_EFFORT_BUDGET_BUILTIN_DEF, LLM_RESOLVED_OPTIONS_BUILTIN_DEF,
    LLM_RESOLVE_MODEL_BUILTIN_DEF,
};

pub(crate) use self::capability_projection::capabilities_to_vm_value;
pub(crate) use self::catalog_builtins::parse_catalog_refresh_options;
pub(crate) use self::model_projection::llm_catalog_value;
pub(crate) use self::provider_projection::llm_provider_status_value;

/// Register config-based LLM builtins (llm_infer_provider, llm_resolve_model, etc.).
pub(crate) fn register_config_builtins(vm: &mut Vm) {
    register_builtin_defs(vm, option_registry::OPTION_REGISTRY_DEFS);
    register_builtin_defs(vm, LLM_CONFIG_DEFS);
}

const LLM_CONFIG_DEFS: &[&VmBuiltinDef] = &[
    &PROVIDER_CAPABILITIES_BUILTIN_DEF,
    &PROVIDER_CAPABILITIES_INSTALL_BUILTIN_DEF,
    &PROVIDER_CAPABILITIES_CLEAR_BUILTIN_DEF,
    &LLM_INFER_PROVIDER_BUILTIN_DEF,
    &LLM_MODEL_TIER_BUILTIN_DEF,
    &LLM_RESOLVE_MODEL_BUILTIN_DEF,
    &LLM_EXECUTION_CONTRACT_BUILTIN_DEF,
    &LLM_MODEL_INFO_BUILTIN_DEF,
    &LLM_KNOWN_MODELS_BUILTIN_DEF,
    &LLM_AVAILABLE_PROVIDERS_BUILTIN_DEF,
    &LLM_QC_DEFAULT_MODEL_BUILTIN_DEF,
    &LLM_RESOLVED_OPTIONS_BUILTIN_DEF,
    &LLM_APPLY_REASONING_POLICY_BUILTIN_DEF,
    &LLM_REASONING_EFFORT_BUDGET_BUILTIN_DEF,
    &LLM_MODEL_DEFAULTS_BUILTIN_DEF,
    &LLM_PROVIDER_CATALOG_BUILTIN_DEF,
    &LLM_PICK_MODEL_BUILTIN_DEF,
    &LLM_COMPLEMENTARY_REVIEWER_BUILTIN_DEF,
    &LLM_PROVIDERS_BUILTIN_DEF,
    &PROVIDER_REGISTER_BUILTIN_DEF,
    &LLM_CONFIG_BUILTIN_DEF,
    &LLM_CATALOG_BUILTIN_DEF,
    &LLM_EQUIVALENT_MODELS_BUILTIN_DEF,
    &LLM_CATALOG_REFRESH_BUILTIN_DEF,
    &LLM_PROVIDER_STATUS_BUILTIN_DEF,
    &LLM_RATE_LIMIT_BUILTIN_DEF,
    &LLM_HEALTHCHECK_BUILTIN_DEF,
];

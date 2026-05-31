//! Channel guardrail builtin registration (CH-11 / #1911).
//!
//! Exposes the thread-local guardrail registry to Harn scripts:
//!
//! * `channel_guardrail_register(config_dict) -> id_string`
//! * `channel_guardrail_unregister(id_string) -> bool`
//! * `channel_guardrail_list() -> [string]`
//! * `channel_guardrail_clear()` — test helper, drains every entry.
//!
//! The actual scanning + verdict logic lives in
//! `crates/harn-vm/src/channel_guardrails.rs`. See that module's
//! doc-comment for the verdict semantics, built-in scanner kinds, and
//! the security/privacy trust contract.

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn err(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(std::sync::Arc::from(message.into())))
}

pub(crate) fn register_channel_guardrail_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &CHANNEL_GUARDRAIL_REGISTER_IMPL_DEF,
    &CHANNEL_GUARDRAIL_UNREGISTER_IMPL_DEF,
    &CHANNEL_GUARDRAIL_LIST_IMPL_DEF,
    &CHANNEL_GUARDRAIL_CLEAR_IMPL_DEF,
];

#[harn_builtin(
    sig = "channel_guardrail_register(config: dict) -> string",
    category = "channel_guardrails"
)]
fn channel_guardrail_register_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let config = match args.first() {
        Some(VmValue::Dict(d)) => d.as_ref().clone(),
        Some(other) => {
            return Err(err(format!(
                "channel_guardrail_register: config must be a dict, got {}",
                other.type_name()
            )));
        }
        None => return Err(err("channel_guardrail_register: requires a config dict")),
    };
    let id = crate::channel_guardrails::register(&config)?;
    Ok(VmValue::String(std::sync::Arc::from(id)))
}

#[harn_builtin(
    sig = "channel_guardrail_unregister(id: string) -> bool",
    category = "channel_guardrails"
)]
fn channel_guardrail_unregister_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let id = match args.first() {
        Some(VmValue::String(s)) => s.to_string(),
        Some(other) => {
            return Err(err(format!(
                "channel_guardrail_unregister: id must be a string, got {}",
                other.type_name()
            )));
        }
        None => return Err(err("channel_guardrail_unregister: requires an id string")),
    };
    Ok(VmValue::Bool(crate::channel_guardrails::unregister(&id)))
}

#[harn_builtin(
    sig = "channel_guardrail_list() -> list",
    category = "channel_guardrails"
)]
fn channel_guardrail_list_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let ids = crate::channel_guardrails::list_ids();
    let values: Vec<VmValue> = ids
        .into_iter()
        .map(|id| VmValue::String(std::sync::Arc::from(id)))
        .collect();
    Ok(VmValue::List(std::sync::Arc::new(values)))
}

#[harn_builtin(
    sig = "channel_guardrail_clear() -> nil",
    category = "channel_guardrails"
)]
fn channel_guardrail_clear_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    crate::channel_guardrails::clear();
    Ok(VmValue::Nil)
}

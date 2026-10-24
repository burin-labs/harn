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

use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

fn err(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::string(message.into()))
}

harn_vm_core::stdlib_module_installer!(Pure, pub(crate) fn register_channel_guardrail_builtins);

#[harn_builtin(
    exposure = "harness.channels.guardrail_register",
    effects = ["state.mutate@const=channel-guardrails"],
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
    Ok(VmValue::string(id))
}

#[harn_builtin(
    exposure = "harness.channels.guardrail_unregister",
    effects = ["state.mutate@const=channel-guardrails"],
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
    exposure = "harness.channels.guardrail_list",
    effects = ["state.read@const=channel-guardrails"],
    sig = "channel_guardrail_list() -> list",
    category = "channel_guardrails"
)]
fn channel_guardrail_list_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let ids = crate::channel_guardrails::list_ids();
    let values: Vec<VmValue> = ids.into_iter().map(VmValue::string).collect();
    Ok(VmValue::list(values))
}

#[harn_builtin(
    exposure = "harness.channels.guardrail_clear",
    effects = ["state.mutate@const=channel-guardrails"],
    sig = "channel_guardrail_clear() -> nil",
    category = "channel_guardrails"
)]
fn channel_guardrail_clear_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    crate::channel_guardrails::clear();
    Ok(VmValue::Nil)
}

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

use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn err(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(Rc::from(message.into())))
}

pub(crate) fn register_channel_guardrail_builtins(vm: &mut Vm) {
    vm.register_builtin("channel_guardrail_register", |args, _out| {
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
        Ok(VmValue::String(Rc::from(id)))
    });

    vm.register_builtin("channel_guardrail_unregister", |args, _out| {
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
    });

    vm.register_builtin("channel_guardrail_list", |_args, _out| {
        let ids = crate::channel_guardrails::list_ids();
        let values: Vec<VmValue> = ids
            .into_iter()
            .map(|id| VmValue::String(Rc::from(id)))
            .collect();
        Ok(VmValue::List(Rc::new(values)))
    });

    vm.register_builtin("channel_guardrail_clear", |_args, _out| {
        crate::channel_guardrails::clear();
        Ok(VmValue::Nil)
    });
}

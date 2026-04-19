use std::collections::BTreeMap;
use std::rc::Rc;

use crate::bridge::json_result_to_vm_value;
use crate::connectors::{active_connector_client, ClientError};
use crate::llm::vm_value_to_json;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_connector_builtins(vm: &mut Vm) {
    vm.register_async_builtin("connector_call", |args| async move {
        let provider = required_string_arg(&args, 0, "connector_call", "provider")?;
        let method = required_string_arg(&args, 1, "connector_call", "method")?;
        let params = match args.get(2) {
            Some(VmValue::Dict(dict)) => vm_value_to_json(&VmValue::Dict(dict.clone())),
            Some(value) if !matches!(value, VmValue::Nil) => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "connector_call: params must be a dict when provided",
                ))));
            }
            _ => vm_value_to_json(&VmValue::Dict(Rc::new(BTreeMap::new()))),
        };

        let client = active_connector_client(&provider).ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "connector_call: connector `{provider}` is not active"
            ))))
        })?;

        let result = client
            .call(&method, params)
            .await
            .map_err(client_error_to_vm)?;
        Ok(json_result_to_vm_value(&result))
    });
}

fn required_string_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    label: &str,
) -> Result<String, VmError> {
    let value = args.get(index).map(VmValue::display).unwrap_or_default();
    if value.trim().is_empty() {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{builtin}: {label} is required"
        )))));
    }
    Ok(value)
}

fn client_error_to_vm(error: ClientError) -> VmError {
    VmError::Thrown(VmValue::String(Rc::from(error.to_string())))
}

//! Typed boundary helpers for Harn agent-host primitive arguments.

use std::sync::Arc;

use crate::value::{DictMap, VmError, VmValue};

pub(super) fn tools(
    args: &[VmValue],
    index: usize,
    label: &str,
) -> Result<Option<VmValue>, VmError> {
    match args.get(index) {
        Some(VmValue::Nil) | None => Ok(crate::stdlib::tools::current_tool_registry()),
        Some(VmValue::Dict(_)) => Ok(args.get(index).cloned()),
        Some(other) => Err(invalid_tools(label, other)),
    }
}

pub(super) fn tools_value(value: Option<VmValue>, label: &str) -> Result<Option<VmValue>, VmError> {
    match value {
        Some(VmValue::Nil) | None => Ok(crate::stdlib::tools::current_tool_registry()),
        Some(value @ VmValue::Dict(_)) => Ok(Some(value)),
        Some(other) => Err(invalid_tools(label, &other)),
    }
}

pub(super) fn options_value(value: Option<VmValue>, label: &str) -> Result<DictMap, VmError> {
    match value {
        Some(VmValue::Dict(options)) => {
            Ok(Arc::try_unwrap(options).unwrap_or_else(|options| options.as_ref().clone()))
        }
        Some(VmValue::Nil) | None => Ok(DictMap::new()),
        Some(other) => Err(VmError::Runtime(format!(
            "{label}: options must be a dict or nil; got {}",
            other.type_name()
        ))),
    }
}

pub(super) fn option_str(options: &DictMap, key: &str) -> Option<String> {
    match options.get(key)? {
        VmValue::Nil => None,
        value => Some(value.display()),
    }
}

pub(super) fn option_int(options: &DictMap, key: &str) -> Option<i64> {
    options.get(key)?.as_int()
}

fn invalid_tools(label: &str, value: &VmValue) -> VmError {
    VmError::Runtime(format!(
        "{label}: tools must be a tool registry dict or nil; got {}",
        value.type_name()
    ))
}

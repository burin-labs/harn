//! Runtime parameter contracts for portable calls.

use crate::type_contract::{self, RuntimeTypeKind, TypeContractValue};
use crate::{CompiledFunction, Diagnostic};

use super::{diagnostic, RuntimeValue};

impl TypeContractValue for RuntimeValue {
    fn runtime_type_kind(&self) -> RuntimeTypeKind {
        match self {
            Self::Nil => RuntimeTypeKind::Nil,
            Self::Bool(_) => RuntimeTypeKind::Bool,
            Self::Int(_) => RuntimeTypeKind::Int,
            Self::Float(_) => RuntimeTypeKind::Float,
            Self::String(_) => RuntimeTypeKind::String,
            Self::Bytes(_) => RuntimeTypeKind::Bytes,
            Self::List(_) => RuntimeTypeKind::List,
            Self::Record(_) => RuntimeTypeKind::Dict,
            Self::Enum(_) => RuntimeTypeKind::Enum,
            Self::Closure(_) | Self::Builtin(_) => RuntimeTypeKind::Closure,
            Self::Harness(_) => RuntimeTypeKind::Harness,
        }
    }

    fn list_items(&self) -> Option<&[Self]> {
        match self {
            Self::List(items) => Some(items),
            _ => None,
        }
    }

    fn record_field(&self, name: &str) -> Option<&Self> {
        match self {
            Self::Record(fields) => fields.get(name),
            _ => None,
        }
    }

    fn record_values_match(&self, predicate: &mut dyn FnMut(&Self) -> bool) -> Option<bool> {
        match self {
            Self::Record(fields) => Some(fields.values().all(predicate)),
            _ => None,
        }
    }

    fn string_literal(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    fn int_literal(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    fn nominal_type_name(&self) -> Option<&str> {
        match self {
            Self::Enum(value) => Some(&value.enum_name),
            _ => None,
        }
    }
}

pub(super) fn validate_call(
    function: &CompiledFunction,
    arguments: &[RuntimeValue],
) -> Result<(), Diagnostic> {
    let minimum = if function.has_rest_param {
        function
            .required_param_count()
            .min(function.params.len().saturating_sub(1))
    } else {
        function.required_param_count()
    };
    let maximum = (!function.has_rest_param).then_some(function.params.len());
    if arguments.len() < minimum || maximum.is_some_and(|limit| arguments.len() > limit) {
        return Err(diagnostic(
            "arity_mismatch",
            format!(
                "function `{}` expected {}..{} arguments, got {}",
                function.name,
                minimum,
                maximum.map_or_else(|| "unbounded".to_string(), |value| value.to_string()),
                arguments.len()
            ),
        ));
    }
    if !function.has_runtime_type_checks {
        return Ok(());
    }
    for (index, value) in arguments.iter().enumerate() {
        let parameter =
            if function.has_rest_param && index >= function.params.len().saturating_sub(1) {
                function.params.last()
            } else {
                function.params.get(index)
            };
        let Some(parameter) = parameter else {
            continue;
        };
        let Some(expected) = &parameter.type_expr else {
            continue;
        };
        if !type_contract::matches_type(
            value,
            expected,
            &function.type_params,
            &function.nominal_type_names,
        ) {
            return Err(diagnostic(
                "argument_type",
                format!(
                    "function `{}` parameter `{}` rejected {}",
                    function.name,
                    parameter.name,
                    runtime_type_name(value)
                ),
            ));
        }
    }
    Ok(())
}

fn runtime_type_name(value: &RuntimeValue) -> &'static str {
    match value.runtime_type_kind() {
        RuntimeTypeKind::Nil => "nil",
        RuntimeTypeKind::Bool => "bool",
        RuntimeTypeKind::Int => "int",
        RuntimeTypeKind::Float => "float",
        RuntimeTypeKind::String => "string",
        RuntimeTypeKind::Bytes => "bytes",
        RuntimeTypeKind::List => "list",
        RuntimeTypeKind::Dict => "dict",
        RuntimeTypeKind::Closure => "closure",
        RuntimeTypeKind::Harness => "Harness",
        RuntimeTypeKind::Enum => "enum",
        _ => "unsupported portable value",
    }
}

/// Check an annotated `let` / `const` initializer against its declared type.
///
/// The binding-site counterpart of [`validate_call`], and the same decision
/// procedure: both run the value through `type_contract::matches_type`. A
/// declared type therefore accepts and rejects the same values wherever it is
/// written (harn#6252).
pub(super) fn validate_binding(
    value: &RuntimeValue,
    slot: &crate::BindingTypeSlot,
) -> Result<(), Diagnostic> {
    if type_contract::matches_type(value, &slot.type_expr, &[], &slot.nominal_type_names) {
        return Ok(());
    }
    Err(diagnostic(
        "binding_type",
        format!(
            "binding `{}` rejected {}",
            slot.name,
            runtime_type_name(value)
        ),
    ))
}

use std::rc::Rc;
use std::sync::Arc;

use super::resource::{MAX_VALUE_BYTES, MAX_VALUE_NODES};
use super::runtime_value::RuntimeValue;
use super::{diagnostic, runtime_value_kind};
use crate::Diagnostic;

fn numeric(
    left: RuntimeValue,
    right: RuntimeValue,
    ints: fn(i64, i64) -> Option<i64>,
    floats: fn(f64, f64) -> f64,
) -> Result<RuntimeValue, Diagnostic> {
    Ok(match (left, right) {
        (RuntimeValue::Int(a), RuntimeValue::Int(b)) => ints(a, b).map_or_else(
            || RuntimeValue::Float(floats(a as f64, b as f64)),
            RuntimeValue::Int,
        ),
        (RuntimeValue::Int(a), RuntimeValue::Float(b)) => RuntimeValue::Float(floats(a as f64, b)),
        (RuntimeValue::Float(a), RuntimeValue::Int(b)) => RuntimeValue::Float(floats(a, b as f64)),
        (RuntimeValue::Float(a), RuntimeValue::Float(b)) => RuntimeValue::Float(floats(a, b)),
        (a, b) => {
            return Err(diagnostic(
                "numeric_type",
                format!(
                    "cannot apply numeric operation to {} and {}",
                    runtime_value_kind(&a),
                    runtime_value_kind(&b)
                ),
            ));
        }
    })
}

pub(super) fn add(a: RuntimeValue, b: RuntimeValue) -> Result<RuntimeValue, Diagnostic> {
    match (a, b) {
        (RuntimeValue::String(a), RuntimeValue::String(b)) => {
            let length = a.len().checked_add(b.len()).ok_or_else(|| {
                diagnostic("value_byte_limit", "string concatenation length overflow")
            })?;
            if length > MAX_VALUE_BYTES {
                return Err(diagnostic(
                    "value_byte_limit",
                    "string concatenation exceeds the portable value byte limit",
                ));
            }
            let mut value = String::with_capacity(length);
            value.push_str(&a);
            value.push_str(&b);
            Ok(RuntimeValue::String(Arc::from(value)))
        }
        (RuntimeValue::List(a), RuntimeValue::List(b)) => {
            let length = a.len().checked_add(b.len()).ok_or_else(|| {
                diagnostic("value_node_limit", "list concatenation length overflow")
            })?;
            if length > MAX_VALUE_NODES {
                return Err(diagnostic(
                    "value_node_limit",
                    "list concatenation exceeds the portable value node limit",
                ));
            }
            let mut values = Vec::with_capacity(length);
            values.extend(a.iter().cloned());
            values.extend(b.iter().cloned());
            Ok(RuntimeValue::List(Rc::new(values)))
        }
        (RuntimeValue::Record(a), RuntimeValue::Record(b)) => {
            if a.len().saturating_add(b.len()) > MAX_VALUE_NODES {
                return Err(diagnostic(
                    "value_node_limit",
                    "record merge exceeds the portable value node limit",
                ));
            }
            let mut values = (*a).clone();
            values.extend(b.iter().map(|(key, value)| (key.clone(), value.clone())));
            Ok(RuntimeValue::Record(Rc::new(values)))
        }
        (a, b) => numeric(a, b, i64::checked_add, |a, b| a + b),
    }
}

pub(super) fn sub(a: RuntimeValue, b: RuntimeValue) -> Result<RuntimeValue, Diagnostic> {
    numeric(a, b, i64::checked_sub, |a, b| a - b)
}

pub(super) fn mul(a: RuntimeValue, b: RuntimeValue) -> Result<RuntimeValue, Diagnostic> {
    numeric(a, b, i64::checked_mul, |a, b| a * b)
}

pub(super) fn div(a: RuntimeValue, b: RuntimeValue) -> Result<RuntimeValue, Diagnostic> {
    if matches!((&a, &b), (RuntimeValue::Int(_), RuntimeValue::Int(0))) {
        return Err(diagnostic("division_by_zero", "integer division by zero"));
    }
    numeric(a, b, i64::checked_div, |a, b| a / b)
}

pub(super) fn modulo(a: RuntimeValue, b: RuntimeValue) -> Result<RuntimeValue, Diagnostic> {
    if matches!((&a, &b), (RuntimeValue::Int(_), RuntimeValue::Int(0))) {
        return Err(diagnostic("division_by_zero", "integer modulo by zero"));
    }
    numeric(a, b, |a, b| Some(a.wrapping_rem(b)), |a, b| a % b)
}

pub(super) fn pow(a: RuntimeValue, b: RuntimeValue) -> Result<RuntimeValue, Diagnostic> {
    match (a, b) {
        (RuntimeValue::Int(a), RuntimeValue::Int(b)) if b >= 0 => {
            let exponent = u32::try_from(b).map_err(|_| {
                diagnostic("numeric_range", "integer exponent is outside the u32 range")
            })?;
            Ok(a.checked_pow(exponent).map_or_else(
                || RuntimeValue::Float((a as f64).powf(exponent as f64)),
                RuntimeValue::Int,
            ))
        }
        (RuntimeValue::Int(a), RuntimeValue::Int(b)) => {
            Ok(RuntimeValue::Float((a as f64).powf(b as f64)))
        }
        (a, b) => numeric(a, b, |_, _| None, f64::powf),
    }
}

pub(super) fn negate(value: RuntimeValue) -> Result<RuntimeValue, Diagnostic> {
    match value {
        RuntimeValue::Int(value) => Ok(value
            .checked_neg()
            .map_or(RuntimeValue::Float(-(value as f64)), RuntimeValue::Int)),
        RuntimeValue::Float(value) => Ok(RuntimeValue::Float(-value)),
        value => Err(diagnostic(
            "numeric_type",
            format!("cannot negate {}", runtime_value_kind(&value)),
        )),
    }
}

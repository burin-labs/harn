use super::{diagnostic, DataValue, RuntimeValue};
use crate::Diagnostic;

pub(super) const MAX_VALUE_NODES: usize = 100_000;
const MAX_VALUE_DEPTH: usize = 128;
pub(super) const MAX_VALUE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub(super) struct ResourceUsage {
    pub(super) nodes: usize,
}

pub(super) fn validate_json_value(root: &serde_json::Value) -> Result<(), Diagnostic> {
    let mut stack = vec![(root, 1_usize)];
    let mut nodes = 1_usize;
    let mut bytes = 0_usize;
    while let Some((value, depth)) = stack.pop() {
        check_depth(depth, "portable value")?;
        match value {
            serde_json::Value::String(value) => bytes = bytes.saturating_add(value.len()),
            serde_json::Value::Array(values) => {
                reserve_children(values.len(), depth, &mut nodes, "portable value")?;
                stack.extend(values.iter().map(|value| (value, depth + 1)));
            }
            serde_json::Value::Object(entries) => {
                reserve_children(entries.len(), depth, &mut nodes, "portable value")?;
                for (key, value) in entries {
                    bytes = bytes.saturating_add(key.len());
                    stack.push((value, depth + 1));
                }
            }
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            }
        }
        check_bytes(bytes, "portable value")?;
    }
    Ok(())
}

pub(super) fn validate_data_value(root: &DataValue) -> Result<(), Diagnostic> {
    let mut stack = vec![(root, 1_usize)];
    let mut nodes = 1_usize;
    let mut bytes = 0_usize;
    while let Some((value, depth)) = stack.pop() {
        check_depth(depth, "portable value")?;
        match value {
            DataValue::String(value) => bytes = bytes.saturating_add(value.len()),
            DataValue::Bytes(value) => bytes = bytes.saturating_add(value.len()),
            DataValue::List(values) => {
                reserve_children(values.len(), depth, &mut nodes, "portable value")?;
                stack.extend(values.iter().map(|value| (value, depth + 1)));
            }
            DataValue::Record(entries) => {
                reserve_children(entries.len(), depth, &mut nodes, "portable value")?;
                for (key, value) in entries {
                    bytes = bytes.saturating_add(key.len());
                    stack.push((value, depth + 1));
                }
            }
            DataValue::Nil | DataValue::Bool(_) | DataValue::Int(_) | DataValue::Float(_) => {}
        }
        check_bytes(bytes, "portable value")?;
    }
    Ok(())
}

pub(super) fn validate_runtime_value(root: &RuntimeValue) -> Result<ResourceUsage, Diagnostic> {
    let mut stack = vec![(root, 1_usize)];
    let mut nodes = 1_usize;
    let mut bytes = 0_usize;
    while let Some((value, depth)) = stack.pop() {
        check_depth(depth, "runtime value")?;
        match value {
            RuntimeValue::String(value) => bytes = bytes.saturating_add(value.len()),
            RuntimeValue::Bytes(value) => bytes = bytes.saturating_add(value.len()),
            RuntimeValue::List(values) => {
                reserve_children(values.len(), depth, &mut nodes, "runtime value")?;
                stack.extend(values.iter().map(|value| (value, depth + 1)));
            }
            RuntimeValue::Record(entries) => {
                reserve_children(entries.len(), depth, &mut nodes, "runtime value")?;
                for (key, value) in entries.iter() {
                    bytes = bytes.saturating_add(key.len());
                    stack.push((value, depth + 1));
                }
            }
            RuntimeValue::Enum(value) => {
                bytes = bytes
                    .saturating_add(value.enum_name.len())
                    .saturating_add(value.variant.len());
                reserve_children(value.fields.len(), depth, &mut nodes, "runtime value")?;
                stack.extend(value.fields.iter().map(|value| (value, depth + 1)));
            }
            RuntimeValue::Nil
            | RuntimeValue::Bool(_)
            | RuntimeValue::Int(_)
            | RuntimeValue::Float(_)
            | RuntimeValue::Closure(_)
            | RuntimeValue::Builtin(_)
            | RuntimeValue::Harness(_) => {}
        }
        check_bytes(bytes, "runtime value")?;
    }
    Ok(ResourceUsage { nodes })
}

fn check_depth(depth: usize, subject: &str) -> Result<(), Diagnostic> {
    if depth > MAX_VALUE_DEPTH {
        return Err(diagnostic(
            "value_depth_limit",
            format!("{subject} exceeds the maximum nesting depth"),
        ));
    }
    Ok(())
}

fn reserve_children(
    count: usize,
    parent_depth: usize,
    nodes: &mut usize,
    subject: &str,
) -> Result<(), Diagnostic> {
    if count != 0 {
        check_depth(parent_depth.saturating_add(1), subject)?;
    }
    *nodes = nodes.saturating_add(count);
    if *nodes > MAX_VALUE_NODES {
        return Err(diagnostic(
            "value_node_limit",
            format!("{subject} exceeds the maximum node count"),
        ));
    }
    Ok(())
}

fn check_bytes(bytes: usize, subject: &str) -> Result<(), Diagnostic> {
    if bytes > MAX_VALUE_BYTES {
        return Err(diagnostic(
            "value_byte_limit",
            format!("{subject} exceeds the maximum string/byte budget"),
        ));
    }
    Ok(())
}

use super::*;

pub(super) fn binary(
    frame: &mut Frame,
    operation: fn(RuntimeValue, RuntimeValue) -> Result<RuntimeValue, Diagnostic>,
) -> Result<RuntimeValue, OpStep> {
    let right = frame
        .stack
        .pop()
        .ok_or_else(|| OpStep::Error(diagnostic("stack_underflow", "binary rhs missing")))?;
    let left = frame
        .stack
        .pop()
        .ok_or_else(|| OpStep::Error(diagnostic("stack_underflow", "binary lhs missing")))?;
    operation(left, right).map_err(OpStep::Error)
}
pub(super) fn compare(
    machine: &mut Machine<'_>,
    frame: &mut Frame,
    predicate: fn(i8) -> bool,
) -> Result<(), OpStep> {
    let right = frame
        .stack
        .pop()
        .ok_or_else(|| OpStep::Error(diagnostic("stack_underflow", "comparison rhs missing")))?;
    let left = frame
        .stack
        .pop()
        .ok_or_else(|| OpStep::Error(diagnostic("stack_underflow", "comparison lhs missing")))?;
    machine
        .charge_values_work(&[&left, &right])
        .map_err(OpStep::Error)?;
    let value = ordering(&left, &right).map(predicate).unwrap_or(false);
    frame.stack.push(RuntimeValue::Bool(value));
    Ok(())
}

pub(super) fn compare_equality(
    machine: &mut Machine<'_>,
    frame: &mut Frame,
    expected: bool,
) -> Result<(), OpStep> {
    let right = frame
        .stack
        .pop()
        .ok_or_else(|| OpStep::Error(diagnostic("stack_underflow", "comparison rhs missing")))?;
    let left = frame
        .stack
        .pop()
        .ok_or_else(|| OpStep::Error(diagnostic("stack_underflow", "comparison lhs missing")))?;
    let equal = machine.values_equal(&left, &right).map_err(OpStep::Error)?;
    frame.stack.push(RuntimeValue::Bool(equal == expected));
    Ok(())
}
pub(super) fn equal(a: &RuntimeValue, b: &RuntimeValue) -> bool {
    semantic_values_equal(a, b)
}
pub(super) fn ordering(a: &RuntimeValue, b: &RuntimeValue) -> Option<i8> {
    semantic_try_compare(a, b)
}
pub(super) fn get_property(value: &RuntimeValue, name: &str) -> Option<RuntimeValue> {
    match value {
        RuntimeValue::Record(values) => values.get(name).cloned(),
        RuntimeValue::Enum(value) if name == "variant" => {
            Some(RuntimeValue::String(value.variant.clone()))
        }
        RuntimeValue::Enum(value) if name == "fields" => {
            Some(RuntimeValue::List(value.fields.clone()))
        }
        RuntimeValue::List(values) if name == "count" => {
            Some(RuntimeValue::Int(values.len() as i64))
        }
        RuntimeValue::String(value) if name == "count" => {
            Some(RuntimeValue::Int(value.chars().count() as i64))
        }
        RuntimeValue::Harness(root) if root == "root" => {
            Some(RuntimeValue::Harness(name.to_string()))
        }
        _ => None,
    }
}

pub(super) fn set_property_value(
    target: RuntimeValue,
    property: &str,
    value: RuntimeValue,
) -> Result<RuntimeValue, Diagnostic> {
    let RuntimeValue::Record(values) = target else {
        return Err(diagnostic(
            "property_assignment",
            format!("cannot set property `{property}` on a non-record value"),
        ));
    };
    let mut updated = Rc::unwrap_or_clone(values);
    updated.insert(property.to_string(), value);
    Ok(RuntimeValue::Record(Rc::new(updated)))
}

pub(super) fn set_subscript_value(
    target: RuntimeValue,
    index: RuntimeValue,
    value: RuntimeValue,
) -> Result<RuntimeValue, Diagnostic> {
    match target {
        RuntimeValue::List(values) => {
            let RuntimeValue::Int(index) = index else {
                return Err(diagnostic(
                    "subscript_assignment",
                    "list assignment requires an integer index",
                ));
            };
            let Some(index) = normalized_index(values.len(), index) else {
                return Err(diagnostic(
                    "subscript_assignment",
                    "list assignment index is out of bounds",
                ));
            };
            let mut updated = Rc::unwrap_or_clone(values);
            updated[index] = value;
            Ok(RuntimeValue::List(Rc::new(updated)))
        }
        RuntimeValue::Record(values) => {
            let mut updated = Rc::unwrap_or_clone(values);
            updated.insert(index.display(), value);
            Ok(RuntimeValue::Record(Rc::new(updated)))
        }
        _ => Err(diagnostic(
            "subscript_assignment",
            "only lists and records support subscript assignment",
        )),
    }
}
pub(super) fn slice(
    value: RuntimeValue,
    start: RuntimeValue,
    end: RuntimeValue,
) -> Result<RuntimeValue, Diagnostic> {
    match value {
        RuntimeValue::List(values) => {
            let (start, end) = slice_bounds(values.len(), start, end)?;
            Ok(RuntimeValue::List(Rc::new(values[start..end].to_vec())))
        }
        RuntimeValue::String(value) => {
            let chars: Vec<_> = value.chars().collect();
            let (start, end) = slice_bounds(chars.len(), start, end)?;
            Ok(RuntimeValue::String(Arc::from(
                chars[start..end].iter().collect::<String>(),
            )))
        }
        _ => Err(diagnostic(
            "slice_type",
            "slice receiver must be list or string",
        )),
    }
}

pub(super) fn normalized_index(length: usize, index: i64) -> Option<usize> {
    let length = i64::try_from(length).ok()?;
    let index = if index < 0 {
        length.checked_add(index)?
    } else {
        index
    };
    (0..length).contains(&index).then_some(index as usize)
}

pub(super) fn slice_bounds(
    length: usize,
    start: RuntimeValue,
    end: RuntimeValue,
) -> Result<(usize, usize), Diagnostic> {
    let length = i64::try_from(length)
        .map_err(|_| diagnostic("slice_range", "slice receiver is too large"))?;
    let bound = |value: RuntimeValue, default: i64, label: &str| match value {
        RuntimeValue::Nil => Ok(default),
        RuntimeValue::Int(value) if value < 0 => Ok((length + value).max(0)),
        RuntimeValue::Int(value) => Ok(value.min(length)),
        _ => Err(diagnostic(
            "slice_type",
            format!("slice {label} must be int or nil"),
        )),
    };
    let start = bound(start, 0, "start")?;
    let end = bound(end, length, "end")?;
    if start >= end {
        Ok((0, 0))
    } else {
        Ok((start as usize, end as usize))
    }
}
pub(super) fn runtime_value_kind(value: &RuntimeValue) -> &'static str {
    match value {
        RuntimeValue::Nil => "nil",
        RuntimeValue::Bool(_) => "bool",
        RuntimeValue::Int(_) => "int",
        RuntimeValue::Float(_) => "float",
        RuntimeValue::String(_) => "string",
        RuntimeValue::Bytes(_) => "bytes",
        RuntimeValue::List(_) => "list",
        RuntimeValue::Record(_) => "record",
        RuntimeValue::Enum(_) => "enum",
        RuntimeValue::Closure(_) => "closure",
        RuntimeValue::Builtin(_) => "builtin",
        RuntimeValue::Harness(_) => "harness",
    }
}
pub(super) fn handle_throw(frames: &mut Vec<Frame>, value: RuntimeValue) -> bool {
    while let Some(frame) = frames.last_mut() {
        if let Some(handler) = frame.handlers.pop() {
            frame.stack.truncate(handler.stack_depth);
            frame.env = handler.env;
            frame.stack.push(value);
            frame.ip = handler.target;
            return true;
        }
        frames.pop();
    }
    false
}

pub(super) fn request_id(
    digest: [u8; 32],
    ordinal: u64,
    capability: &str,
    operation: &str,
    arguments: &DataValue,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&digest);
    hasher.update(&ordinal.to_be_bytes());
    hasher.update(capability.as_bytes());
    hasher.update(&[0]);
    hasher.update(operation.as_bytes());
    hasher.update(&serde_json::to_vec(arguments).unwrap_or_default());
    hasher.finalize().to_hex()[..32].to_string()
}
pub(super) fn diagnostic(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        code: code.to_string(),
        message: message.into(),
        line: None,
        column: None,
    }
}
pub(super) fn failed(code: &str, message: impl Into<String>) -> Execution {
    Execution::Failed {
        diagnostic: diagnostic(code, message),
    }
}

use std::collections::BTreeMap;
use std::fmt;

use crate::environment::Environment;
use harn_parser::{Node, TypeExpr};

/// Runtime values in the Harn interpreter.
#[derive(Debug, Clone)]
pub enum Value {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Nil,
    List(Vec<Value>),
    Dict(BTreeMap<String, Value>),
    Closure {
        params: Vec<String>,
        param_types: Vec<Option<TypeExpr>>,
        return_type: Option<TypeExpr>,
        body: Vec<Node>,
        env: Environment,
    },
    TaskHandle {
        id: String,
    },
    /// An enum variant value: EnumName.Variant(fields...)
    EnumVariant {
        enum_name: String,
        variant: String,
        fields: Vec<Value>,
    },
    /// A struct instance: StructName { field: value, ... }
    StructInstance {
        struct_name: String,
        fields: BTreeMap<String, Value>,
    },
}

impl Value {
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Nil => false,
            Value::Int(n) => *n != 0,
            Value::Float(n) => *n != 0.0,
            Value::String(s) => !s.is_empty(),
            Value::List(items) => !items.is_empty(),
            Value::Dict(map) => !map.is_empty(),
            Value::Closure { .. } => true,
            Value::TaskHandle { .. } => true,
            Value::EnumVariant { .. } => true,
            Value::StructInstance { fields, .. } => !fields.is_empty(),
        }
    }

    pub fn as_string(&self) -> String {
        if let Value::String(s) = self {
            s.clone()
        } else {
            self.to_string()
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        if let Value::Int(n) = self {
            Some(*n)
        } else {
            None
        }
    }

    pub fn as_list(&self) -> Option<&Vec<Value>> {
        if let Value::List(items) = self {
            Some(items)
        } else {
            None
        }
    }

    pub fn as_dict(&self) -> Option<&BTreeMap<String, Value>> {
        if let Value::Dict(map) = self {
            Some(map)
        } else {
            None
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::String(s) => write!(f, "{s}"),
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(n) => {
                // Match Swift's default float formatting
                if *n == (*n as i64) as f64 && n.abs() < 1e15 {
                    write!(f, "{:.1}", n)
                } else {
                    write!(f, "{n}")
                }
            }
            Value::Bool(b) => write!(f, "{}", if *b { "true" } else { "false" }),
            Value::Nil => write!(f, "nil"),
            Value::List(items) => {
                write!(f, "[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "]")
            }
            Value::Dict(map) => {
                write!(f, "{{")?;
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            Value::Closure { params, .. } => {
                write!(f, "<fn({})>", params.join(", "))
            }
            Value::TaskHandle { id } => write!(f, "<task:{id}>"),
            Value::EnumVariant {
                enum_name,
                variant,
                fields,
            } => {
                if fields.is_empty() {
                    write!(f, "{enum_name}.{variant}")
                } else {
                    let inner: Vec<String> = fields.iter().map(|v| v.to_string()).collect();
                    write!(f, "{enum_name}.{variant}({})", inner.join(", "))
                }
            }
            Value::StructInstance {
                struct_name,
                fields,
            } => {
                let inner: Vec<String> = fields.iter().map(|(k, v)| format!("{k}: {v}")).collect();
                write!(f, "{struct_name} {{{}}}", inner.join(", "))
            }
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Nil, Value::Nil) => true,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Dict(a), Value::Dict(b)) => a == b,
            (Value::Closure { .. }, Value::Closure { .. }) => false,
            (Value::TaskHandle { id: a }, Value::TaskHandle { id: b }) => a == b,
            (
                Value::EnumVariant {
                    enum_name: a,
                    variant: b,
                    fields: c,
                },
                Value::EnumVariant {
                    enum_name: d,
                    variant: e,
                    fields: f,
                },
            ) => a == d && b == e && c == f,
            (
                Value::StructInstance {
                    struct_name: a,
                    fields: b,
                },
                Value::StructInstance {
                    struct_name: c,
                    fields: d,
                },
            ) => a == c && b == d,
            _ => false,
        }
    }
}

/// Compare values for equality (with int/float cross-comparison).
pub fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Nil, Value::Nil) => true,
        (Value::Int(x), Value::Float(y)) => (*x as f64) == *y,
        (Value::Float(x), Value::Int(y)) => *x == (*y as f64),
        (
            Value::EnumVariant {
                enum_name: a_e,
                variant: a_v,
                fields: a_f,
            },
            Value::EnumVariant {
                enum_name: b_e,
                variant: b_v,
                fields: b_f,
            },
        ) => {
            a_e == b_e
                && a_v == b_v
                && a_f.len() == b_f.len()
                && a_f.iter().zip(b_f.iter()).all(|(x, y)| values_equal(x, y))
        }
        (
            Value::StructInstance {
                struct_name: a_s,
                fields: a_f,
            },
            Value::StructInstance {
                struct_name: b_s,
                fields: b_f,
            },
        ) => a_s == b_s && a_f == b_f,
        _ => false,
    }
}

/// Compare values for ordering. Returns -1, 0, or 1.
/// Supports cross-type int/float comparison by promoting int to float.
pub fn compare_values(a: &Value, b: &Value) -> i32 {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y) as i32,
        (Value::Float(x), Value::Float(y)) => cmp_f64(*x, *y),
        (Value::Int(x), Value::Float(y)) => cmp_f64(*x as f64, *y),
        (Value::Float(x), Value::Int(y)) => cmp_f64(*x, *y as f64),
        (Value::String(x), Value::String(y)) => x.cmp(y) as i32,
        _ => 0,
    }
}

fn cmp_f64(a: f64, b: f64) -> i32 {
    if a < b {
        -1
    } else if a > b {
        1
    } else {
        0
    }
}

/// Check if a value matches a type expression. Returns Ok(()) or an error message.
/// `type_registry` maps named types to their definitions (from `type Name = ...`).
pub fn check_type(value: &Value, type_expr: &TypeExpr, context: &str) -> Result<(), String> {
    check_type_with_registry(value, type_expr, context, &BTreeMap::new())
}

/// Check type with a type registry for resolving named type aliases.
pub fn check_type_with_registry(
    value: &Value,
    type_expr: &TypeExpr,
    context: &str,
    registry: &BTreeMap<String, TypeExpr>,
) -> Result<(), String> {
    match type_expr {
        TypeExpr::Named(name) => {
            // Check if it's a user-defined type alias
            if let Some(resolved) = registry.get(name) {
                return check_type_with_registry(value, resolved, context, registry);
            }
            let actual = value_type_name(value);
            if actual == name.as_str() {
                Ok(())
            } else {
                Err(format!(
                    "Type error at {context}: expected {name}, got {actual}"
                ))
            }
        }
        TypeExpr::Union(types) => {
            for t in types {
                if check_type_with_registry(value, t, context, registry).is_ok() {
                    return Ok(());
                }
            }
            let expected: Vec<String> = types.iter().map(type_expr_name).collect();
            Err(format!(
                "Type error at {context}: expected {}, got {}",
                expected.join(" | "),
                value_type_name(value)
            ))
        }
        TypeExpr::Shape(fields) => {
            let map = match value {
                Value::Dict(map) => map,
                _ => {
                    return Err(format!(
                        "Type error at {context}: expected dict shape, got {}",
                        value_type_name(value)
                    ))
                }
            };
            for field in fields {
                match map.get(&field.name) {
                    Some(val) => check_type_with_registry(
                        val,
                        &field.type_expr,
                        &format!("{context}.{}", field.name),
                        registry,
                    )?,
                    None if field.optional => {} // OK, optional field absent
                    None => {
                        return Err(format!(
                            "Type error at {context}: missing required field '{}'",
                            field.name
                        ))
                    }
                }
            }
            Ok(())
        }
        TypeExpr::List(inner) => {
            if let Value::List(items) = value {
                for (i, item) in items.iter().enumerate() {
                    check_type_with_registry(item, inner, &format!("{context}[{i}]"), registry)?;
                }
                Ok(())
            } else {
                Err(format!(
                    "Type error at {context}: expected list, got {}",
                    value_type_name(value)
                ))
            }
        }
    }
}

/// Get the type name of a runtime value.
pub fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Bool(_) => "bool",
        Value::Nil => "nil",
        Value::List(_) => "list",
        Value::Dict(_) => "dict",
        Value::Closure { .. } => "closure",
        Value::TaskHandle { .. } => "taskHandle",
        Value::EnumVariant { .. } => "enum",
        Value::StructInstance { .. } => "struct",
    }
}

fn type_expr_name(t: &TypeExpr) -> String {
    match t {
        TypeExpr::Named(n) => n.clone(),
        TypeExpr::Union(types) => types
            .iter()
            .map(type_expr_name)
            .collect::<Vec<_>>()
            .join(" | "),
        TypeExpr::Shape(_) => "{...}".to_string(),
        TypeExpr::List(inner) => format!("list[{}]", type_expr_name(inner)),
    }
}

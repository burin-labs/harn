use std::collections::BTreeMap;
use std::fmt;

use crate::environment::Environment;
use harn_parser::Node;

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
        body: Vec<Node>,
        env: Environment,
    },
    TaskHandle {
        id: String,
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
        _ => false,
    }
}

/// Compare values for ordering. Returns -1, 0, or 1.
pub fn compare_values(a: &Value, b: &Value) -> i32 {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => {
            if x < y {
                -1
            } else if x > y {
                1
            } else {
                0
            }
        }
        (Value::Float(x), Value::Float(y)) => {
            if x < y {
                -1
            } else if x > y {
                1
            } else {
                0
            }
        }
        (Value::String(x), Value::String(y)) => {
            if x < y {
                -1
            } else if x > y {
                1
            } else {
                0
            }
        }
        _ => 0,
    }
}

use crate::value::Value;
use std::fmt;

/// Runtime errors.
#[derive(Debug, Clone)]
pub enum RuntimeError {
    UndefinedVariable(String),
    UndefinedBuiltin(String),
    ImmutableAssignment(String),
    TypeMismatch { expected: String, got: Value },
    ReturnValue(Option<Value>),
    RetryExhausted,
    ThrownError(Value),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::UndefinedVariable(name) => {
                write!(f, "Undefined variable: {name}")
            }
            RuntimeError::UndefinedBuiltin(name) => {
                write!(f, "Undefined builtin: {name}")
            }
            RuntimeError::ImmutableAssignment(name) => {
                write!(f, "Cannot assign to immutable binding: {name}")
            }
            RuntimeError::TypeMismatch { expected, got } => {
                write!(f, "Type mismatch: expected {expected}, got {got}")
            }
            RuntimeError::ReturnValue(_) => write!(f, "Return from pipeline"),
            RuntimeError::RetryExhausted => write!(f, "All retry attempts exhausted"),
            RuntimeError::ThrownError(value) => write!(f, "Thrown: {}", value.as_string()),
        }
    }
}

impl std::error::Error for RuntimeError {}

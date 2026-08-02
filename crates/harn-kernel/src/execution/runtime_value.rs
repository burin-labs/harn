use std::collections::BTreeMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use super::resource::validate_runtime_value;
use super::{diagnostic, DataValue, Env};
use crate::value::{SemanticValue, ValueView};
use crate::{CompiledFunction, Constant, Diagnostic};

#[derive(Clone)]
pub(super) struct Closure {
    pub(super) function: Arc<CompiledFunction>,
    pub(super) env: Weak<Env>,
}

#[derive(Clone)]
pub(super) enum RuntimeValue {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(Arc<str>),
    Bytes(Arc<[u8]>),
    List(Rc<Vec<RuntimeValue>>),
    Record(Rc<BTreeMap<String, RuntimeValue>>),
    Closure(Closure),
    Builtin(String),
    Harness(String),
}

impl RuntimeValue {
    pub(super) fn truthy(&self) -> bool {
        match self {
            Self::Nil | Self::Bool(false) => false,
            Self::Int(0) => false,
            Self::Float(value) if *value == 0.0 || value.is_nan() => false,
            Self::String(value) => !value.is_empty(),
            Self::List(value) => !value.is_empty(),
            Self::Record(value) => !value.is_empty(),
            _ => true,
        }
    }

    pub(super) fn display(&self) -> String {
        match self {
            Self::Nil => "nil".into(),
            Self::Bool(value) => value.to_string(),
            Self::Int(value) => value.to_string(),
            Self::Float(value) => value.to_string(),
            Self::String(value) => value.to_string(),
            Self::Bytes(value) => format!("<{} bytes>", value.len()),
            Self::List(values) => format!(
                "[{}]",
                values
                    .iter()
                    .map(Self::display)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Record(values) => format!(
                "{{{}}}",
                values
                    .iter()
                    .map(|(key, value)| format!("{key}: {}", value.display()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Closure(_) => "<closure>".into(),
            Self::Builtin(name) => format!("<builtin {name}>"),
            Self::Harness(name) => format!("<harness {name}>"),
        }
    }
}

impl SemanticValue for RuntimeValue {
    fn semantic_view(&self) -> ValueView<'_, Self> {
        match self {
            Self::Nil => ValueView::Nil,
            Self::Bool(value) => ValueView::Bool(*value),
            Self::Int(value) => ValueView::Int(*value),
            Self::Float(value) => ValueView::Float(*value),
            Self::String(value) => ValueView::String(value),
            Self::Bytes(value) => ValueView::Bytes(value),
            Self::List(values) => ValueView::List(values),
            Self::Record(values) => ValueView::Record(values),
            Self::Closure(_) | Self::Builtin(_) | Self::Harness(_) => ValueView::Opaque,
        }
    }
}

impl From<DataValue> for RuntimeValue {
    fn from(value: DataValue) -> Self {
        match value {
            DataValue::Nil => Self::Nil,
            DataValue::Bool(value) => Self::Bool(value),
            DataValue::Int(value) => Self::Int(value),
            DataValue::Float(value) => Self::Float(value),
            DataValue::String(value) => Self::String(Arc::from(value)),
            DataValue::Bytes(value) => Self::Bytes(Arc::from(value)),
            DataValue::List(values) => {
                Self::List(Rc::new(values.into_iter().map(Self::from).collect()))
            }
            DataValue::Record(values) => Self::Record(Rc::new(
                values
                    .into_iter()
                    .map(|(key, value)| (key, Self::from(value)))
                    .collect(),
            )),
        }
    }
}

impl From<Constant> for RuntimeValue {
    fn from(value: Constant) -> Self {
        match value {
            Constant::Int(value) => Self::Int(value),
            Constant::Float(value) => Self::Float(value),
            Constant::String(value) => Self::String(Arc::from(value)),
            Constant::Bool(value) => Self::Bool(value),
            Constant::Nil => Self::Nil,
            Constant::Duration(value) => Self::Int(value),
        }
    }
}

impl TryFrom<RuntimeValue> for DataValue {
    type Error = Diagnostic;

    fn try_from(value: RuntimeValue) -> Result<Self, Self::Error> {
        validate_runtime_value(&value)?;
        Self::try_from_validated(value)
    }
}

impl DataValue {
    fn try_from_validated(value: RuntimeValue) -> Result<Self, Diagnostic> {
        Ok(match value {
            RuntimeValue::Nil => Self::Nil,
            RuntimeValue::Bool(value) => Self::Bool(value),
            RuntimeValue::Int(value) => Self::Int(value),
            RuntimeValue::Float(value) => Self::Float(value),
            RuntimeValue::String(value) => Self::String(value.to_string()),
            RuntimeValue::Bytes(value) => Self::Bytes(value.to_vec()),
            RuntimeValue::List(values) => Self::List(
                Rc::unwrap_or_clone(values)
                    .into_iter()
                    .map(Self::try_from_validated)
                    .collect::<Result<_, _>>()?,
            ),
            RuntimeValue::Record(values) => Self::Record(
                Rc::unwrap_or_clone(values)
                    .into_iter()
                    .map(|(key, value)| Ok((key, Self::try_from_validated(value)?)))
                    .collect::<Result<_, Diagnostic>>()?,
            ),
            RuntimeValue::Closure(_) | RuntimeValue::Builtin(_) | RuntimeValue::Harness(_) => {
                return Err(diagnostic(
                    "non_data_result",
                    "execution returned a host or callable value",
                ));
            }
        })
    }
}

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

pub type DictMap = BTreeMap<String, VmValue>;
pub type HarnStr = arcstr::ArcStr;

/// Borrowed projection used by the compiler and portable executor's shared
/// authority-free value semantics.
///
/// Keeping equality and ordering generic over this small view avoids a second
/// recursive implementation for runtime values while still allowing the
/// executor to retain closures and host handles outside the data vocabulary.
pub(crate) enum ValueView<'a, T> {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(&'a str),
    Bytes(&'a [u8]),
    Duration(i64),
    List(&'a [T]),
    Record(&'a BTreeMap<String, T>),
    Enum {
        enum_name: &'a str,
        variant: &'a str,
        fields: &'a [T],
    },
    Opaque,
}

pub(crate) trait SemanticValue: Sized {
    fn semantic_view(&self) -> ValueView<'_, Self>;
}

/// Authority-free value vocabulary used by artifacts, snapshots, and hosts.
#[derive(Debug, Clone)]
pub enum VmValue {
    Int(i64),
    Float(f64),
    String(HarnStr),
    Bool(bool),
    Nil,
    Duration(i64),
    List(Arc<Vec<VmValue>>),
    Dict(Arc<DictMap>),
}

impl VmValue {
    pub fn dict<K>(entries: impl IntoIterator<Item = (K, VmValue)>) -> Self
    where
        K: Into<String>,
    {
        Self::Dict(Arc::new(
            entries
                .into_iter()
                .map(|(key, value)| (key.into(), value))
                .collect(),
        ))
    }

    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Nil | Self::Bool(false) => false,
            Self::Int(value) => *value != 0,
            Self::Float(value) => *value != 0.0 && !value.is_nan(),
            Self::String(value) => !value.is_empty(),
            Self::List(value) => !value.is_empty(),
            Self::Dict(value) => !value.is_empty(),
            Self::Bool(true) | Self::Duration(_) => true,
        }
    }

    pub fn display(&self) -> String {
        self.to_string()
    }
}

impl SemanticValue for VmValue {
    fn semantic_view(&self) -> ValueView<'_, Self> {
        match self {
            Self::Nil => ValueView::Nil,
            Self::Bool(value) => ValueView::Bool(*value),
            Self::Int(value) => ValueView::Int(*value),
            Self::Float(value) => ValueView::Float(*value),
            Self::String(value) => ValueView::String(value),
            Self::Duration(value) => ValueView::Duration(*value),
            Self::List(values) => ValueView::List(values),
            Self::Dict(values) => ValueView::Record(values),
        }
    }
}

impl fmt::Display for VmValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(value) => write!(formatter, "{value}"),
            Self::Float(value) => write!(formatter, "{value}"),
            Self::String(value) => formatter.write_str(value),
            Self::Bool(value) => write!(formatter, "{value}"),
            Self::Nil => formatter.write_str("nil"),
            Self::Duration(value) => write!(formatter, "{value}ms"),
            Self::List(values) => {
                formatter.write_str("[")?;
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(", ")?;
                    }
                    write!(formatter, "{value}")?;
                }
                formatter.write_str("]")
            }
            Self::Dict(entries) => {
                formatter.write_str("{")?;
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(", ")?;
                    }
                    write!(formatter, "{key}: {value}")?;
                }
                formatter.write_str("}")
            }
        }
    }
}

pub fn values_equal(left: &VmValue, right: &VmValue) -> bool {
    semantic_values_equal(left, right)
}

pub fn try_compare_values(left: &VmValue, right: &VmValue) -> Option<i8> {
    semantic_try_compare(left, right)
}

/// Structural equality for any authority-free value projection.
///
/// The explicit work stack makes compiler constant folding safe for deeply
/// nested source values and lets the executor charge the whole value graph
/// once before evaluating it without consuming the Rust call stack.
pub(crate) fn semantic_values_equal<T: SemanticValue>(left: &T, right: &T) -> bool {
    let mut pending = vec![(left, right)];
    while let Some((left, right)) = pending.pop() {
        match (left.semantic_view(), right.semantic_view()) {
            (ValueView::Nil, ValueView::Nil) => {}
            (ValueView::Bool(left), ValueView::Bool(right)) if left == right => {}
            (ValueView::Int(left), ValueView::Int(right)) if left == right => {}
            (ValueView::Float(left), ValueView::Float(right)) if left == right => {}
            (ValueView::Int(left), ValueView::Float(right)) if left as f64 == right => {}
            (ValueView::Float(left), ValueView::Int(right)) if left == right as f64 => {}
            (ValueView::String(left), ValueView::String(right)) if left == right => {}
            (ValueView::Bytes(left), ValueView::Bytes(right)) if left == right => {}
            (ValueView::Duration(left), ValueView::Duration(right)) if left == right => {}
            (ValueView::List(left), ValueView::List(right)) if left.len() == right.len() => {
                pending.extend(left.iter().zip(right.iter()));
            }
            (ValueView::Record(left), ValueView::Record(right)) if left.len() == right.len() => {
                for (key, left_value) in left {
                    let Some(right_value) = right.get(key) else {
                        return false;
                    };
                    pending.push((left_value, right_value));
                }
            }
            (
                ValueView::Enum {
                    enum_name: left_enum,
                    variant: left_variant,
                    fields: left_fields,
                },
                ValueView::Enum {
                    enum_name: right_enum,
                    variant: right_variant,
                    fields: right_fields,
                },
            ) if left_enum == right_enum
                && left_variant == right_variant
                && left_fields.len() == right_fields.len() =>
            {
                pending.extend(left_fields.iter().zip(right_fields.iter()));
            }
            _ => return false,
        }
    }
    true
}

enum CompareTask<'a, T> {
    Values(&'a T, &'a T),
    ListLength(usize, usize),
}

/// Canonical ordering for the pure value vocabulary.
///
/// Lists compare lexicographically. NaN is unordered and propagates through a
/// containing list. Other non-orderable values retain Harn's established
/// comparison behavior and compare equal for relational purposes; equality
/// itself remains structural through [`semantic_values_equal`].
pub(crate) fn semantic_try_compare<T: SemanticValue>(left: &T, right: &T) -> Option<i8> {
    use std::cmp::Ordering;

    let mut pending = vec![CompareTask::Values(left, right)];
    while let Some(task) = pending.pop() {
        let ordering = match task {
            CompareTask::ListLength(left, right) => left.cmp(&right),
            CompareTask::Values(left, right) => match (left.semantic_view(), right.semantic_view())
            {
                (ValueView::Int(left), ValueView::Int(right)) => left.cmp(&right),
                (ValueView::Float(left), ValueView::Float(right)) => left.partial_cmp(&right)?,
                (ValueView::Int(left), ValueView::Float(right)) => {
                    (left as f64).partial_cmp(&right)?
                }
                (ValueView::Float(left), ValueView::Int(right)) => {
                    left.partial_cmp(&(right as f64))?
                }
                (ValueView::String(left), ValueView::String(right)) => left.cmp(right),
                (ValueView::List(left), ValueView::List(right)) => {
                    pending.push(CompareTask::ListLength(left.len(), right.len()));
                    for (left, right) in left.iter().zip(right.iter()).rev() {
                        pending.push(CompareTask::Values(left, right));
                    }
                    continue;
                }
                _ => Ordering::Equal,
            },
        };
        match ordering {
            Ordering::Less => return Some(-1),
            Ordering::Greater => return Some(1),
            Ordering::Equal => {}
        }
    }
    Some(0)
}

pub fn intern_key(key: &str) -> String {
    key.to_owned()
}

pub trait VmDictExt {
    fn put_str(&mut self, key: &str, value: &str);
}

impl VmDictExt for DictMap {
    fn put_str(&mut self, key: &str, value: &str) {
        self.insert(key.to_string(), VmValue::String(value.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(values: Vec<VmValue>) -> VmValue {
        VmValue::List(Arc::new(values))
    }

    #[test]
    fn shared_ordering_is_iterative_lexicographic_and_nan_aware() {
        assert_eq!(
            try_compare_values(
                &list(vec![VmValue::Int(1), VmValue::Int(2)]),
                &list(vec![VmValue::Int(1), VmValue::Int(3)]),
            ),
            Some(-1)
        );
        assert_eq!(
            try_compare_values(
                &list(vec![VmValue::Int(1)]),
                &list(vec![VmValue::Int(1), VmValue::Int(0)])
            ),
            Some(-1)
        );
        assert_eq!(
            try_compare_values(
                &list(vec![VmValue::Float(f64::NAN)]),
                &list(vec![VmValue::Int(1)]),
            ),
            None
        );
    }

    #[test]
    fn non_orderable_values_retain_native_relational_fallback() {
        assert_eq!(
            try_compare_values(&VmValue::Bool(false), &VmValue::Bool(true)),
            Some(0)
        );
        assert_eq!(
            try_compare_values(&VmValue::Nil, &VmValue::Dict(Arc::new(BTreeMap::new()))),
            Some(0)
        );
    }
}

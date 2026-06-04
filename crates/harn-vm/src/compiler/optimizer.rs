use std::collections::BTreeMap;

use harn_parser::{DictEntry, Node, SNode};

use crate::runtime_limits::RuntimeLimits;
use crate::value::{try_compare_values, values_equal, VmValue};

const MAX_FOLDED_STRING_BYTES: usize = RuntimeLimits::DEFAULT.max_constant_folded_string_bytes;
const MAX_FOLDED_COLLECTION_ITEMS: usize =
    RuntimeLimits::DEFAULT.max_constant_folded_collection_items;

pub(super) fn fold_constant_expr(expr: &SNode) -> Option<SNode> {
    if !is_constant_fold_candidate(&expr.node) {
        return None;
    }
    value_to_snode(constant_value(expr)?, expr.span)
}

fn is_constant_fold_candidate(node: &Node) -> bool {
    matches!(
        node,
        Node::BinaryOp { .. }
            | Node::UnaryOp { .. }
            | Node::Ternary { .. }
            | Node::ListLiteral(_)
            | Node::DictLiteral(_)
    )
}

fn constant_value(expr: &SNode) -> Option<VmValue> {
    match &expr.node {
        Node::IntLiteral(value) => Some(VmValue::Int(*value)),
        Node::FloatLiteral(value) => Some(VmValue::Float(*value)),
        Node::StringLiteral(value) | Node::RawStringLiteral(value) => {
            Some(VmValue::String(std::sync::Arc::from(value.as_str())))
        }
        Node::BoolLiteral(value) => Some(VmValue::Bool(*value)),
        Node::NilLiteral => Some(VmValue::Nil),
        Node::DurationLiteral(ms) => i64::try_from(*ms).ok().map(VmValue::Duration),
        Node::UnaryOp { op, operand } => fold_unary(op, constant_value(operand)?),
        Node::BinaryOp { op, left, right } => {
            fold_binary(op, constant_value(left)?, constant_value(right)?)
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            if constant_value(condition)?.is_truthy() {
                constant_value(true_expr)
            } else {
                constant_value(false_expr)
            }
        }
        Node::ListLiteral(items) => {
            if items.len() > MAX_FOLDED_COLLECTION_ITEMS {
                return None;
            }
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                if matches!(item.node, Node::Spread(_)) {
                    return None;
                }
                values.push(constant_value(item)?);
            }
            Some(VmValue::List(std::sync::Arc::new(values)))
        }
        Node::DictLiteral(entries) => {
            if entries.len() > MAX_FOLDED_COLLECTION_ITEMS {
                return None;
            }
            let mut values = BTreeMap::new();
            for entry in entries {
                if matches!(entry.value.node, Node::Spread(_)) {
                    return None;
                }
                values.insert(
                    constant_value(&entry.key)?.display(),
                    constant_value(&entry.value)?,
                );
            }
            Some(VmValue::Dict(std::sync::Arc::new(values)))
        }
        _ => None,
    }
}

fn fold_unary(op: &str, value: VmValue) -> Option<VmValue> {
    match (op, value) {
        ("-", VmValue::Int(value)) => Some(VmValue::Int(value.wrapping_neg())),
        ("-", VmValue::Float(value)) => Some(VmValue::Float(-value)),
        ("!", value) => Some(VmValue::Bool(!value.is_truthy())),
        _ => None,
    }
}

fn fold_binary(op: &str, left: VmValue, right: VmValue) -> Option<VmValue> {
    match op {
        "+" => fold_add(left, right),
        "-" => fold_sub(left, right),
        "*" => fold_mul(left, right),
        "/" => fold_div(left, right),
        "%" => fold_mod(left, right),
        "**" => fold_pow(left, right),
        "==" => Some(VmValue::Bool(values_equal(&left, &right))),
        "!=" => Some(VmValue::Bool(!values_equal(&left, &right))),
        // Mirror the runtime: NaN is unordered, so every relational operator is
        // `false` when `try_compare_values` returns `None`.
        "<" => Some(VmValue::Bool(
            matches!(try_compare_values(&left, &right), Some(o) if o < 0),
        )),
        ">" => Some(VmValue::Bool(
            matches!(try_compare_values(&left, &right), Some(o) if o > 0),
        )),
        "<=" => Some(VmValue::Bool(
            matches!(try_compare_values(&left, &right), Some(o) if o <= 0),
        )),
        ">=" => Some(VmValue::Bool(
            matches!(try_compare_values(&left, &right), Some(o) if o >= 0),
        )),
        "&&" => Some(VmValue::Bool(left.is_truthy() && right.is_truthy())),
        "||" => Some(VmValue::Bool(left.is_truthy() || right.is_truthy())),
        "??" => {
            if matches!(left, VmValue::Nil) {
                Some(right)
            } else {
                Some(left)
            }
        }
        "in" => Some(VmValue::Bool(contains_value(&left, &right))),
        "not_in" => Some(VmValue::Bool(!contains_value(&left, &right))),
        _ => None,
    }
}

fn fold_add(left: VmValue, right: VmValue) -> Option<VmValue> {
    match (left, right) {
        (VmValue::Int(left), VmValue::Int(right)) => Some(VmValue::Int(left.wrapping_add(right))),
        (VmValue::Float(left), VmValue::Float(right)) => Some(VmValue::Float(left + right)),
        (VmValue::Int(left), VmValue::Float(right)) => Some(VmValue::Float(left as f64 + right)),
        (VmValue::Float(left), VmValue::Int(right)) => Some(VmValue::Float(left + right as f64)),
        (VmValue::String(left), VmValue::String(right)) => {
            let len = left.len().checked_add(right.len())?;
            if len > MAX_FOLDED_STRING_BYTES {
                return None;
            }
            let mut out = String::with_capacity(len);
            out.push_str(&left);
            out.push_str(&right);
            Some(VmValue::String(std::sync::Arc::from(out)))
        }
        (VmValue::List(left), VmValue::List(right)) => {
            let len = left.len().checked_add(right.len())?;
            if len > MAX_FOLDED_COLLECTION_ITEMS {
                return None;
            }
            let mut out = Vec::with_capacity(len);
            out.extend(left.iter().cloned());
            out.extend(right.iter().cloned());
            Some(VmValue::List(std::sync::Arc::new(out)))
        }
        (VmValue::Dict(left), VmValue::Dict(right)) => {
            let len = left.len().checked_add(right.len())?;
            if len > MAX_FOLDED_COLLECTION_ITEMS {
                return None;
            }
            let mut out = left.as_ref().clone();
            out.extend(
                right
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
            Some(VmValue::Dict(std::sync::Arc::new(out)))
        }
        _ => None,
    }
}

fn fold_sub(left: VmValue, right: VmValue) -> Option<VmValue> {
    match (left, right) {
        (VmValue::Int(left), VmValue::Int(right)) => Some(VmValue::Int(left.wrapping_sub(right))),
        (VmValue::Float(left), VmValue::Float(right)) => Some(VmValue::Float(left - right)),
        (VmValue::Int(left), VmValue::Float(right)) => Some(VmValue::Float(left as f64 - right)),
        (VmValue::Float(left), VmValue::Int(right)) => Some(VmValue::Float(left - right as f64)),
        _ => None,
    }
}

fn fold_mul(left: VmValue, right: VmValue) -> Option<VmValue> {
    match (left, right) {
        (VmValue::Int(left), VmValue::Int(right)) => Some(VmValue::Int(left.wrapping_mul(right))),
        (VmValue::Float(left), VmValue::Float(right)) => Some(VmValue::Float(left * right)),
        (VmValue::Int(left), VmValue::Float(right)) => Some(VmValue::Float(left as f64 * right)),
        (VmValue::Float(left), VmValue::Int(right)) => Some(VmValue::Float(left * right as f64)),
        (VmValue::String(text), VmValue::Int(count))
        | (VmValue::Int(count), VmValue::String(text)) => {
            let count = usize::try_from(count.max(0)).ok()?;
            let len = text.len().checked_mul(count)?;
            if len > MAX_FOLDED_STRING_BYTES {
                return None;
            }
            Some(VmValue::String(std::sync::Arc::from(text.repeat(count))))
        }
        _ => None,
    }
}

fn fold_div(left: VmValue, right: VmValue) -> Option<VmValue> {
    match (left, right) {
        (VmValue::Int(_), VmValue::Int(0)) => None,
        (VmValue::Int(left), VmValue::Int(right)) => left.checked_div(right).map(VmValue::Int),
        (VmValue::Float(left), VmValue::Float(right)) => Some(VmValue::Float(left / right)),
        (VmValue::Int(left), VmValue::Float(right)) => Some(VmValue::Float(left as f64 / right)),
        (VmValue::Float(left), VmValue::Int(right)) => Some(VmValue::Float(left / right as f64)),
        _ => None,
    }
}

fn fold_mod(left: VmValue, right: VmValue) -> Option<VmValue> {
    match (left, right) {
        (VmValue::Int(_), VmValue::Int(0)) => None,
        (VmValue::Int(left), VmValue::Int(right)) => left.checked_rem(right).map(VmValue::Int),
        (VmValue::Float(_), VmValue::Float(0.0)) => None,
        (VmValue::Float(left), VmValue::Float(right)) => Some(VmValue::Float(left % right)),
        (VmValue::Int(_), VmValue::Float(0.0)) => None,
        (VmValue::Int(left), VmValue::Float(right)) => Some(VmValue::Float(left as f64 % right)),
        (VmValue::Float(_), VmValue::Int(0)) => None,
        (VmValue::Float(left), VmValue::Int(right)) => Some(VmValue::Float(left % right as f64)),
        _ => None,
    }
}

fn fold_pow(left: VmValue, right: VmValue) -> Option<VmValue> {
    match (left, right) {
        (VmValue::Int(base), VmValue::Int(exp)) => {
            if u32::try_from(exp).is_ok() {
                Some(VmValue::Int(base.wrapping_pow(exp as u32)))
            } else {
                Some(VmValue::Float((base as f64).powf(exp as f64)))
            }
        }
        (VmValue::Float(base), VmValue::Int(exp)) => {
            if i32::try_from(exp).is_ok() {
                Some(VmValue::Float(base.powi(exp as i32)))
            } else {
                Some(VmValue::Float(base.powf(exp as f64)))
            }
        }
        (VmValue::Int(base), VmValue::Float(exp)) => Some(VmValue::Float((base as f64).powf(exp))),
        (VmValue::Float(base), VmValue::Float(exp)) => Some(VmValue::Float(base.powf(exp))),
        _ => None,
    }
}

fn contains_value(item: &VmValue, collection: &VmValue) -> bool {
    match collection {
        VmValue::List(items) => items.iter().any(|value| values_equal(value, item)),
        VmValue::Dict(entries) => entries.contains_key(&item.display()),
        VmValue::String(text) => match item {
            VmValue::String(substr) => text.contains(&**substr),
            other => text.contains(&other.display()),
        },
        _ => false,
    }
}

fn value_to_snode(value: VmValue, span: harn_lexer::Span) -> Option<SNode> {
    let node = match value {
        VmValue::Int(value) => Node::IntLiteral(value),
        VmValue::Float(value) => Node::FloatLiteral(value),
        VmValue::String(value) => Node::StringLiteral(value.to_string()),
        VmValue::Bool(value) => Node::BoolLiteral(value),
        VmValue::Nil => Node::NilLiteral,
        VmValue::Duration(ms) => Node::DurationLiteral(u64::try_from(ms).ok()?),
        VmValue::List(values) => {
            let items = values
                .iter()
                .cloned()
                .map(|value| value_to_snode(value, span))
                .collect::<Option<Vec<_>>>()?;
            Node::ListLiteral(items)
        }
        VmValue::Dict(values) => {
            let entries = values
                .iter()
                .map(|(key, value)| {
                    Some(DictEntry {
                        key: SNode {
                            node: Node::StringLiteral(key.clone()),
                            span,
                        },
                        value: value_to_snode(value.clone(), span)?,
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Node::DictLiteral(entries)
        }
        _ => return None,
    };
    Some(SNode { node, span })
}

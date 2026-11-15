//! Binary-op operand rule plus the `string + expr` → interpolation auto-fix
//! builder.
//!
//! Binary-op diagnostics are emitted by `check_node` as it walks an
//! expression. The pure result-type rule (no diagnostics) lives in
//! `super::super::binary_ops::infer_binary_op_type`.

use crate::ast::*;
use harn_lexer::{FixEdit, Span};

use super::super::binary_ops::infer_binary_op_type;
use super::super::scope::{InferredType, TypeScope};
use super::super::union::{contains_nil, without_nil};
use super::super::TypeChecker;

fn empty_dict_fallback_type(left: &TypeExpr) -> InferredType {
    if !contains_nil(left) {
        return Some(left.clone());
    }
    let mut non_nil = without_nil(left)?;
    match &mut non_nil {
        TypeExpr::DictType(..) => Some(non_nil),
        TypeExpr::Applied { name, .. } if name == "dict" => Some(non_nil),
        TypeExpr::Shape(fields) | TypeExpr::OpenShape { fields, .. } => {
            for field in fields {
                field.optional = true;
            }
            Some(non_nil)
        }
        _ => None,
    }
}

/// Whether `l <op> r` is a valid numeric arithmetic pair. `int` promotes to
/// either `float` or `decimal`, but `float` and `decimal` never mix (binary
/// float would corrupt exact decimals) — mirroring the VM's runtime rule.
pub(in crate::typechecker) fn numeric_binop_ok(l: &str, r: &str) -> bool {
    matches!(
        (l, r),
        ("int" | "float", "int" | "float") | ("int" | "decimal", "int" | "decimal")
    )
}

impl TypeChecker {
    pub(super) fn infer_nil_coalesce_type(
        &self,
        left: &SNode,
        right: &SNode,
        scope: &TypeScope,
    ) -> InferredType {
        // Resolve aliases before removing nil, including aliases of nilable records.
        let left_type = self
            .infer_type(left, scope)
            .map(|ty| self.resolve_alias(&ty, scope));
        if matches!(&right.node, Node::DictLiteral(entries) if entries.is_empty()) {
            if let Some(joined) = left_type.as_ref().and_then(empty_dict_fallback_type) {
                return Some(joined);
            }
        }
        // An empty fallback contributes no elements to a typed collection.
        let right_type = match &right.node {
            Node::ListLiteral(items) if items.is_empty() => {
                Some(TypeExpr::List(Box::new(TypeExpr::Never)))
            }
            _ => self.infer_type(right, scope),
        };
        infer_binary_op_type(BinaryOperator::NilCoalesce, &left_type, &right_type)
    }

    /// Build a fix that converts `"str" + expr` or `expr + "str"` to string interpolation.
    pub(in crate::typechecker) fn build_interpolation_fix(
        &self,
        left: &SNode,
        right: &SNode,
        left_is_string: bool,
        expr_span: Span,
    ) -> Option<Vec<FixEdit>> {
        let src = self.source.as_ref()?;
        let (str_node, other_node) = if left_is_string {
            (left, right)
        } else {
            (right, left)
        };
        let str_text = src.get(str_node.span.start..str_node.span.end)?;
        let other_text = src.get(other_node.span.start..other_node.span.end)?;
        // Only handle simple double-quoted strings (not multiline/raw)
        let inner = str_text.strip_prefix('"')?.strip_suffix('"')?;
        // Skip if the expression contains characters that would break interpolation
        if other_text.contains('}') || other_text.contains('"') {
            return None;
        }
        let replacement = if left_is_string {
            format!("\"{inner}${{{other_text}}}\"")
        } else {
            format!("\"${{{other_text}}}{inner}\"")
        };
        Some(vec![FixEdit {
            span: expr_span,
            replacement,
        }])
    }
}

//! Diagnostic-emitting binary-op checker plus the `string + expr` →
//! interpolation auto-fix builder.
//!
//! `check_binops` walks an expression tree and, for each `BinaryOp` whose
//! operand types are known, verifies that the operator is defined for
//! that type pair. The pure result-type rule (no diagnostics) lives in
//! `super::super::binary_ops::infer_binary_op_type`.

use crate::ast::*;
use crate::diagnostic_codes::Code;
use harn_lexer::{FixEdit, Span};

use super::super::scope::TypeScope;
use super::super::{is_gradual_type_name, TypeChecker};

impl TypeChecker {
    /// Recursively validate binary operations in an expression tree.
    /// Unlike `check_node`, this only checks BinaryOp type compatibility
    /// without triggering other validations (e.g., function call arg checks).
    pub(in crate::typechecker) fn check_binops(&mut self, snode: &SNode, scope: &mut TypeScope) {
        match &snode.node {
            Node::BinaryOp { op, left, right } => {
                self.check_binops(left, scope);
                self.check_binops(right, scope);
                let lt = self.infer_type(left, scope);
                let rt = self.infer_type(right, scope);
                let named_pair = match (&lt, &rt) {
                    // Gradual operands (`any`/`unknown`/`_`) are compatible with
                    // every operator — adding/concatenating a value of unknown
                    // static type is a runtime concern, not a static error.
                    // `number` is the `int | float` alias, so treat it as a
                    // union operand (falls through to the gradual arm) instead
                    // of a concrete name — matching how an explicit
                    // `int | float` operand is already handled.
                    (Some(TypeExpr::Named(l)), Some(TypeExpr::Named(r)))
                        if !is_gradual_type_name(l)
                            && !is_gradual_type_name(r)
                            && l != "number"
                            && r != "number" =>
                    {
                        Some((l, r))
                    }
                    _ => None,
                };
                if let Some((l, r)) = named_pair {
                    let span = snode.span;
                    match op.as_str() {
                        "+" => {
                            let valid = matches!(
                                (l.as_str(), r.as_str()),
                                ("int" | "float", "int" | "float")
                                    | ("string", "string")
                                    | ("list", "list")
                                    | ("dict", "dict")
                            );
                            if !valid {
                                let msg = format!("can't add {l} and {r}");
                                let fix = if l == "string" || r == "string" {
                                    self.build_interpolation_fix(left, right, l == "string", span)
                                } else {
                                    None
                                };
                                if let Some(fix) = fix {
                                    self.error_at_with_fix(
                                        Code::StringInterpolationRewrite,
                                        msg,
                                        span,
                                        fix,
                                    );
                                } else {
                                    self.error_at(Code::InvalidBinaryOperator, msg, span);
                                }
                            }
                        }
                        "-" | "/" | "%" | "**" => {
                            let numeric = ["int", "float"];
                            if !numeric.contains(&l.as_str()) || !numeric.contains(&r.as_str()) {
                                self.error_at(
                                    Code::InvalidBinaryOperator,
                                    format!(
                                        "can't use '{op}' on {l} and {r} (needs numeric operands)"
                                    ),
                                    span,
                                );
                            }
                        }
                        "*" => {
                            let numeric = ["int", "float"];
                            let is_numeric =
                                numeric.contains(&l.as_str()) && numeric.contains(&r.as_str());
                            let is_string_repeat =
                                (l == "string" && r == "int") || (l == "int" && r == "string");
                            if !is_numeric && !is_string_repeat {
                                self.error_at(
                                    Code::InvalidBinaryOperator,
                                    format!("can't multiply {l} and {r} (try string * int)"),
                                    span,
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            // Recurse into sub-expressions that might contain BinaryOps
            Node::UnaryOp { operand, .. } => self.check_binops(operand, scope),
            _ => {}
        }
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

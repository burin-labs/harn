//! The literal-union argument check: reject a compile-time literal argument
//! that is not a member of a homogeneous string/int-literal union parameter —
//! the exact shape the VM lowers to an `enum` schema and enforces at runtime
//! (see `type_expr_to_schema_value` in `harn-vm`, which builds an enum only
//! for a non-empty union of *all* `LitString` or *all* `LitInt`). Reproducing
//! that rejection statically keeps the check-time and runtime verdicts in
//! lockstep.

use crate::ast::*;
use crate::diagnostic_codes::Code;

use super::super::format::format_type;
use super::super::scope::TypeScope;
use super::super::TypeChecker;

impl TypeChecker {
    /// Emit an argument-type-mismatch diagnostic when `arg` is a compile-time
    /// literal that the homogeneous literal-union parameter `expected` does not
    /// permit. Returns `true` when it fired, so the caller can skip the ordinary
    /// subtyping check for that argument and avoid a duplicate diagnostic.
    pub(in crate::typechecker) fn check_literal_union_arg(
        &mut self,
        index: usize,
        param_name: &str,
        arg: &SNode,
        expected: &TypeExpr,
        scope: &TypeScope,
    ) -> bool {
        let resolved = self.resolve_alias(expected, scope);
        let Some(violation) = literal_union_violation(&resolved, arg) else {
            return false;
        };
        self.error_at_with_help(
            Code::ArgumentTypeMismatch,
            format!(
                "argument {} `{}`: {} is not a permitted value of {}",
                index + 1,
                param_name,
                violation.value,
                format_type(expected),
            ),
            arg.span,
            format!("value must be one of [{}]", violation.permitted.join(", ")),
        );
        true
    }
}

/// A literal argument that violates a homogeneous literal-union parameter:
/// the rendered offending `value` and the `permitted` member set, both ready
/// for a diagnostic message.
pub(super) struct LiteralUnionViolation {
    value: String,
    permitted: Vec<String>,
}

/// Reject a compile-time literal argument that is not a member of a
/// homogeneous string/int-literal union parameter. `resolved` is the
/// parameter type with aliases already resolved by the caller. When the
/// argument's value is already known at check time there is no reason to
/// defer that rejection to a runtime `TypeError`, so this reproduces the VM's
/// decision statically and only then.
///
/// This deliberately fires *only* for a literal argument against a
/// pure-literal union. It leaves untouched the gradual concession
/// (`subtyping.rs`) that lets a runtime-valued `string`/`int` flow into a
/// literal slot: those values are unknown statically and stay a runtime
/// concern. Firing exactly when the VM builds an enum keeps the static and
/// runtime verdicts in lockstep — no value is rejected here that the runtime
/// would accept.
fn literal_union_violation(resolved: &TypeExpr, arg: &SNode) -> Option<LiteralUnionViolation> {
    let mut members = Vec::new();
    flatten_union_members(resolved, &mut members);
    if members.is_empty() {
        return None;
    }
    if members.iter().all(|m| matches!(m, TypeExpr::LitString(_))) {
        let value = arg_string_literal(&arg.node)?;
        if members
            .iter()
            .any(|m| matches!(m, TypeExpr::LitString(s) if *s == value))
        {
            return None;
        }
        return Some(LiteralUnionViolation {
            value: format!("\"{value}\""),
            permitted: members.iter().map(format_type).collect(),
        });
    }
    if members.iter().all(|m| matches!(m, TypeExpr::LitInt(_))) {
        let value = arg_int_literal(&arg.node)?;
        if members
            .iter()
            .any(|m| matches!(m, TypeExpr::LitInt(v) if *v == value))
        {
            return None;
        }
        return Some(LiteralUnionViolation {
            value: value.to_string(),
            permitted: members.iter().map(format_type).collect(),
        });
    }
    None
}

/// Flatten nested `Union` members (produced by alias resolution of unions of
/// unions, e.g. `type B = A | "z"`) into one flat list. Non-union types
/// contribute themselves.
fn flatten_union_members(ty: &TypeExpr, out: &mut Vec<TypeExpr>) {
    match ty {
        TypeExpr::Union(members) => {
            for member in members {
                flatten_union_members(member, out);
            }
        }
        other => out.push(other.clone()),
    }
}

/// The compile-time string value of a bare string-literal argument, if any.
/// Interpolated strings are runtime-valued and deliberately excluded.
fn arg_string_literal(node: &Node) -> Option<String> {
    match node {
        Node::StringLiteral(s) | Node::RawStringLiteral(s) => Some(s.clone()),
        _ => None,
    }
}

/// The compile-time integer value of a bare int-literal argument, if any.
fn arg_int_literal(node: &Node) -> Option<i64> {
    match node {
        Node::IntLiteral(v) => Some(*v),
        _ => None,
    }
}

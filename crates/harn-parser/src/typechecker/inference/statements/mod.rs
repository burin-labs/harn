//! Per-statement / per-expression diagnostic walk.
//!
//! `check_node` is the workhorse `match` over `Node` variants — one arm
//! per syntactic construct, each emitting whatever diagnostics that
//! construct's static rules call for. `check_block` chains it across a
//! sequence of statements while tracking unreachable-code detection.
//!
//! Inline pattern helpers (`define_pattern_vars_typed`,
//! `check_pattern_defaults`) and `check_attributes` live here because
//! they are only called from `check_node`'s arms.

use harn_lexer::{FixEdit, Span};
mod access;
mod attributes;
mod blocks;
mod check_node;
mod expressions;
mod name_resolution;
mod patterns;

use crate::ast::*;
use crate::builtin_signatures;
use crate::diagnostic_codes::Code;

use super::super::binary_ops::infer_binary_op_type;
use super::super::exits::stmt_definitely_exits;
use super::super::format::{format_type, is_obvious_type};
use super::super::is_gradual_type_name;
use super::super::schema_inference::schema_type_expr_from_node;
use super::super::scope::{
    EnumDeclInfo, ImplMethodSig, InferredType, InterfaceDeclInfo, PathNarrowing, StructDeclInfo,
    TypeAliasInfo, TypeScope,
};
use super::super::union::{
    collapse_members_opt, contains_nil, discriminant_field, narrow_shape_union_by_tag,
    narrow_to_single, reference_path_key, simplify_union, without_nil, DiscriminantValue,
};
use super::super::{InlayHintInfo, TypeChecker};
use super::decls::CallableDeclarationContext;
use super::flow::{pattern_alternatives, resolve_union_shape_members};

#[derive(Clone, Copy)]
enum UntypedAccessKind {
    Property,
    Subscript,
}

impl UntypedAccessKind {
    fn direct_label(self) -> &'static str {
        match self {
            Self::Property => "Direct property access",
            Self::Subscript => "Direct subscript access",
        }
    }

    fn variable_label(self) -> &'static str {
        match self {
            Self::Property => "Accessing property",
            Self::Subscript => "Subscript access",
        }
    }
}

/// The three runtime forms that dereference a receiver value: a property
/// read (`obj.name`), a subscript (`obj[idx]`), and a method call
/// (`obj.name(..)`). All three fail identically at runtime when the
/// receiver is statically `nil`, may-be-`nil` (a `T | nil` union), or
/// `unknown`. This enum lets a single diagnosis routine phrase the shared
/// error/help for whichever form the author actually wrote, so the
/// nil-safety guidance stays consistent across `.`, `[]`, and `.()`.
/// (`for`-`in` iteration is deliberately NOT here: iterating `nil` is a
/// designed no-op at runtime — see
/// conformance/tests/language/optional_chaining_nil_coalesce.harn.)
#[derive(Clone, Copy)]
enum AccessForm<'a> {
    Property(&'a str),
    Subscript,
    Method(&'a str),
}

impl AccessForm<'_> {
    /// Subject phrase for the "cannot access … on nil" message, e.g.
    /// "property `name`", "an index", "method `greet`".
    fn subject(self) -> String {
        match self {
            Self::Property(name) => format!("property `{name}`"),
            Self::Subscript => "an index".to_string(),
            Self::Method(name) => format!("method `{name}`"),
        }
    }

    /// The `?`-form operator to recommend in help text, e.g.
    /// "the optional access operator `?.name`".
    fn optional_hint(self) -> String {
        match self {
            Self::Property(name) => {
                format!("the optional access operator `?.{name}`")
            }
            Self::Subscript => "the optional subscript operator `?.[…]`".to_string(),
            Self::Method(name) => {
                format!("the optional call operator `?.{name}(…)`")
            }
        }
    }

    /// Leading clause for the "on an `unknown` value" warning, e.g.
    /// "property access `.name`", "subscript access", "method call `.greet()`".
    fn unknown_label(self) -> String {
        match self {
            Self::Property(name) => format!("property access `.{name}`"),
            Self::Subscript => "subscript access".to_string(),
            Self::Method(name) => format!("method call `.{name}()`"),
        }
    }

    /// What the receiver would have to be for the access to succeed,
    /// completing "… will fail at runtime if the value is not {…}".
    fn unknown_requirement(self) -> &'static str {
        match self {
            Self::Property(_) => "a shape with that field",
            Self::Subscript => "a list, dict, or string",
            Self::Method(_) => "a value with that method",
        }
    }

    /// Trailing clause for the nil-guard help, e.g. "before reading fields".
    fn guard_clause(self) -> &'static str {
        match self {
            Self::Property(_) => "before reading fields",
            Self::Subscript => "before indexing",
            Self::Method(_) => "before calling methods",
        }
    }
}

fn attr_key_name(node: &Node) -> Option<&str> {
    match node {
        Node::Identifier(name) | Node::StringLiteral(name) | Node::RawStringLiteral(name) => {
            Some(name.as_str())
        }
        _ => None,
    }
}

fn is_symbol_like(node: &Node) -> bool {
    matches!(
        node,
        Node::Identifier(_) | Node::StringLiteral(_) | Node::RawStringLiteral(_)
    )
}

fn is_string_literal(node: &Node) -> bool {
    matches!(node, Node::StringLiteral(_) | Node::RawStringLiteral(_))
}

fn symbol_like_value(node: &Node) -> Option<&str> {
    match node {
        Node::Identifier(value) | Node::StringLiteral(value) | Node::RawStringLiteral(value) => {
            Some(value.as_str())
        }
        _ => None,
    }
}

fn dict_entry_key_str(key: &SNode) -> Option<String> {
    symbol_like_value(&key.node).map(str::to_string)
}

fn is_trigger_spec(node: &Node) -> bool {
    if is_symbol_like(node) {
        return true;
    }
    matches!(
        node,
        Node::FunctionCall { name, args, .. }
            if name == "schedule" && args.len() == 1 && is_symbol_like(&args[0].node)
    )
}

/// Narrow a union-typed match value by a single arm pattern. Returns
/// the narrowed type, or `None` when the pattern is not a recognised
/// type-narrowing literal. For `OrPattern`, the per-alternative
/// narrowings are combined into a union (deduped) so a two-alternative
/// arm on a three-member literal union refines to a two-member union.
fn narrow_union_by_arm_pattern(pattern: &SNode, members: &[TypeExpr]) -> Option<TypeExpr> {
    let leaves = pattern_alternatives(pattern);
    let mut collected: Vec<TypeExpr> = Vec::new();
    for leaf in &leaves {
        let narrowed = narrow_union_leaf(&leaf.node, members)?;
        match narrowed {
            TypeExpr::Union(inner) => {
                for m in inner {
                    if !collected.contains(&m) {
                        collected.push(m);
                    }
                }
            }
            other => {
                if !collected.contains(&other) {
                    collected.push(other);
                }
            }
        }
    }
    collapse_members_opt(collected, TypeExpr::Union)
}

fn narrow_union_leaf(node: &Node, members: &[TypeExpr]) -> Option<TypeExpr> {
    // Literal pattern against a union containing the exact literal
    // value — narrow to that literal. This is what makes
    // `"pos" | "neg"` on a `"pos" | "neg" | "zero"` union refine
    // correctly: each alternative picks out its literal member.
    match node {
        Node::StringLiteral(s)
            if members
                .iter()
                .any(|m| matches!(m, TypeExpr::LitString(lit) if lit == s)) =>
        {
            return Some(TypeExpr::LitString(s.clone()));
        }
        Node::IntLiteral(v)
            if members
                .iter()
                .any(|m| matches!(m, TypeExpr::LitInt(lit) if lit == v)) =>
        {
            return Some(TypeExpr::LitInt(*v));
        }
        _ => {}
    }
    let type_name = match node {
        Node::NilLiteral => "nil",
        Node::StringLiteral(_) => "string",
        Node::IntLiteral(_) => "int",
        Node::FloatLiteral(_) => "float",
        Node::BoolLiteral(_) => "bool",
        _ => return None,
    };
    narrow_to_single(members, type_name)
}

/// Narrow a tagged shape union by a single arm pattern on its
/// discriminant. For `OrPattern`, the matched shape variants are
/// combined into a union so `"ping" | "pong" -> …` refines `obj` to
/// `{kind:"ping",…} | {kind:"pong",…}` inside the arm.
fn narrow_shape_union_by_arm_pattern(
    pattern: &SNode,
    members: &[TypeExpr],
    property: &str,
) -> Option<TypeExpr> {
    let leaves = pattern_alternatives(pattern);
    let mut matched: Vec<TypeExpr> = Vec::new();
    for leaf in &leaves {
        let tag = match &leaf.node {
            Node::StringLiteral(s) => DiscriminantValue::Str(s.clone()),
            Node::IntLiteral(v) => DiscriminantValue::Int(*v),
            _ => return None,
        };
        let (shape, _) = narrow_shape_union_by_tag(members, property, &tag)?;
        if !matched.contains(&shape) {
            matched.push(shape);
        }
    }
    collapse_members_opt(matched, TypeExpr::Union)
}

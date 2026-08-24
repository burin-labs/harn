//! The one place that decides whether a parameter must carry a type.
//!
//! A parameter with no annotation used to mean `any`, which switches type
//! checking off in both directions: the body may reach for any member, and
//! callers may pass anything. Nothing recovers that type later, so the
//! declaration is a hole rather than a gradual step.
//!
//! Every consumer of that judgement reads it here. The type checker turns a
//! hit into `HARN-TYP-028`, and the `harn fix` annotation repair walks the
//! same list to decide which sites it must fill in. A second predicate
//! elsewhere would let the error and the migration disagree about what needs
//! a type, so there is exactly one.

use crate::ast::{Node, SNode, TypedParam};
use harn_lexer::Span;

/// The declaration form that owns a parameter list.
///
/// Only forms whose parameter types nothing can recover appear here. A closure
/// or lambda parameter is deliberately absent: the checker types those from the
/// expected type at the position where the literal appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationKind {
    /// `fn name(...)`, with or without `pub`.
    Function,
    /// `gen fn name(...)`.
    Generator,
    /// `pipeline name(...)`.
    Pipeline,
    /// `tool name(...)`.
    Tool,
    /// `fn name(...)` inside an `impl` block.
    Method,
    /// A method signature inside an `interface` block.
    InterfaceMethod,
}

impl DeclarationKind {
    /// Product vocabulary for the diagnostic message.
    pub const fn as_str(self) -> &'static str {
        match self {
            DeclarationKind::Function => "function",
            DeclarationKind::Generator => "generator",
            DeclarationKind::Pipeline => "pipeline",
            DeclarationKind::Tool => "tool",
            DeclarationKind::Method => "method",
            DeclarationKind::InterfaceMethod => "interface method",
        }
    }
}

/// One declared parameter that the rule requires an annotation for.
///
/// Owned rather than borrowed so a caller can hold the list while it rewrites
/// the file the parameters came from.
#[derive(Debug, Clone)]
pub struct UnannotatedParam {
    pub kind: DeclarationKind,
    /// Name of the declaration that owns the parameter list.
    pub owner: String,
    /// Source extent of the owning declaration. A repair that has to blame a
    /// later type error on one of these parameters uses it to find them.
    pub owner_span: Span,
    /// Position in the declared parameter list, counting any `self`.
    pub index: usize,
    pub name: String,
    /// Source extent of the parameter, from an optional `...` through any
    /// default value.
    pub span: Span,
    pub has_default: bool,
    pub is_rest: bool,
}

/// Whether this parameter must carry an explicit type.
///
/// A default value does not exempt a parameter: `options = nil` still leaves
/// every other shape a caller may pass unchecked. `self` in an `impl` or
/// `interface` method is exempt because the enclosing header already names its
/// type.
pub fn requires_annotation(kind: DeclarationKind, index: usize, param: &TypedParam) -> bool {
    if param.type_expr.is_some() {
        return false;
    }
    !is_self_receiver(kind, index, param)
}

fn is_self_receiver(kind: DeclarationKind, index: usize, param: &TypedParam) -> bool {
    matches!(
        kind,
        DeclarationKind::Method | DeclarationKind::InterfaceMethod
    ) && index == 0
        && param.name == "self"
}

/// Visit every parameter in `program` that the rule requires a type for,
/// including declarations nested inside another body.
///
/// The walk is pre-order, so an `impl` block is always seen before the methods
/// it owns. That is what lets one pass tell a method apart from a free
/// function without keeping a parallel copy of the AST's child structure.
pub fn walk_unannotated_params(program: &[SNode], visit: &mut impl FnMut(UnannotatedParam)) {
    let mut method_spans: std::collections::HashSet<(usize, usize)> =
        std::collections::HashSet::new();
    crate::visit::walk_program(program, &mut |node| match &node.node {
        Node::ImplBlock { methods, .. } => {
            for method in methods {
                method_spans.insert((method.span.start, method.span.end));
            }
        }
        Node::InterfaceDecl { methods, .. } => {
            for method in methods {
                report(
                    DeclarationKind::InterfaceMethod,
                    &method.name,
                    method.span,
                    &method.params,
                    visit,
                );
            }
        }
        Node::FnDecl {
            name,
            params,
            is_stream,
            ..
        } => {
            let kind = if method_spans.contains(&(node.span.start, node.span.end)) {
                DeclarationKind::Method
            } else if *is_stream {
                DeclarationKind::Generator
            } else {
                DeclarationKind::Function
            };
            report(kind, name, node.span, params, visit);
        }
        Node::Pipeline { name, params, .. } => {
            report(DeclarationKind::Pipeline, name, node.span, params, visit);
        }
        Node::ToolDecl { name, params, .. } => {
            report(DeclarationKind::Tool, name, node.span, params, visit);
        }
        _ => {}
    });
}

/// Collect the same list [`walk_unannotated_params`] visits.
pub fn unannotated_params(program: &[SNode]) -> Vec<UnannotatedParam> {
    let mut found = Vec::new();
    walk_unannotated_params(program, &mut |param| found.push(param));
    found
}

fn report(
    kind: DeclarationKind,
    owner: &str,
    owner_span: Span,
    params: &[TypedParam],
    visit: &mut impl FnMut(UnannotatedParam),
) {
    for (index, param) in params.iter().enumerate() {
        if requires_annotation(kind, index, param) {
            visit(UnannotatedParam {
                kind,
                owner: owner.to_string(),
                owner_span,
                index,
                name: param.name.clone(),
                span: param.span,
                has_default: param.default_value.is_some(),
                is_rest: param.rest,
            });
        }
    }
}

/// Byte offset where `: Type` belongs for this parameter, or `None` when the
/// span does not contain the parameter name (a synthetic parameter, or a span
/// that does not line up with `source`).
///
/// The annotation goes directly after the name, which is after any `...` and
/// before any ` = default`. Both writers of an annotation — the checker's
/// suggested fix and the `harn fix` migration — use this offset, so a
/// parameter can never be annotated in two different places.
pub fn annotation_insert_offset(source: &str, param: &UnannotatedParam) -> Option<usize> {
    let region = source.get(param.span.start..param.span.end)?;
    let mut from = 0usize;
    while let Some(relative) = region.get(from..)?.find(&param.name) {
        let start = from + relative;
        let end = start + param.name.len();
        let before_ok = start == 0
            || !region
                .get(..start)?
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let after_ok = !region
            .get(end..)?
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if before_ok && after_ok {
            return Some(param.span.start + end);
        }
        from = start + param.name.chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// The diagnostic sentence for one hit. Shared so the checker and any other
/// renderer word the rule identically.
pub fn message(found: &UnannotatedParam) -> String {
    format!(
        "{} `{}` parameter `{}` has no type annotation",
        found.kind.as_str(),
        found.owner,
        found.name
    )
}

/// The repair sentence for one hit.
pub fn help(found: &UnannotatedParam) -> String {
    format!(
        "annotate the parameter, for example `{name}: string`, or write `{name}: unknown` and \
         narrow it at the dynamic boundary. `harn fix --apply` infers the type from the body and the \
         call sites.",
        name = found.name
    )
}

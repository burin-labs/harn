//! Naming and placing a capability argument at a call site.
//!
//! Each per-diagnostic capability repair has to answer two questions: which
//! expression names the capability the callee wants, and where in the argument
//! list it belongs. Both answers depend only on the declarations enclosing the
//! call, so they live here rather than in any one repair. Keeping the insertion
//! point in a single function is what lets the leading-prefix invariant hold for
//! every repair that inserts.

use std::collections::BTreeSet;

use harn_lexer::{FixEdit, Span};
use harn_parser::{visit, Node, SNode, TypeExpr};

use super::signature_threading::add_call_argument_edit;

/// Names bound to a capability by the narrowest declaration enclosing `span`.
pub(super) fn capability_carrier_param_names(program: &[SNode], span: Span) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    visit::walk_program(program, &mut |node| {
        let params = match &node.node {
            Node::FnDecl { params, .. }
            | Node::ToolDecl { params, .. }
            | Node::Pipeline { params, .. }
                if node.span.start <= span.start && node.span.end >= span.end =>
            {
                params
            }
            _ => return,
        };
        for param in params {
            let carries = match param.type_expr.as_ref() {
                Some(TypeExpr::Named(name)) => {
                    name == "Harness"
                        || harn_builtin_meta::CapabilityId::from_type_name(name).is_some()
                }
                Some(TypeExpr::Shape(fields)) => fields.iter().any(|field| {
                    matches!(&field.type_expr, TypeExpr::Named(name)
                        if harn_builtin_meta::CapabilityId::from_type_name(name).is_some())
                }),
                _ => false,
            };
            if carries {
                names.insert(param.name.clone());
            }
        }
    });
    names
}

pub(super) fn argument_carries_a_capability(argument: &SNode, carriers: &BTreeSet<String>) -> bool {
    match &argument.node {
        Node::Identifier(name) => carriers.contains(name),
        Node::PropertyAccess { object, .. } => argument_carries_a_capability(object, carriers),
        _ => false,
    }
}

/// Insert a capability argument immediately before the argument at `span`.
///
/// Every per-diagnostic capability repair funnels through here, so this is
/// where the prefix invariant is enforced. A diagnostic names one argument
/// slot, not the callee's declared capability prefix. When a callee takes
/// several capabilities and the call omits all of them, the reported slot is
/// the second or later one; inserting there leaves the preceding ordinary
/// argument sitting in a capability position and shifts every later argument
/// one slot. The call is then wrong rather than incomplete, and the shift
/// resurfaces as an unrelated type error a slot away.
///
/// Capability parameters are always a contiguous leading prefix, so a lone
/// insertion is sound only when every preceding argument already carries a
/// capability. Calls that fail that test belong to the whole-program pass,
/// which reads the callee's declared prefix from the module graph and inserts
/// it whole; when that pass cannot resolve the call, the site is left for a
/// human instead of being silently shifted.
pub(super) fn insert_call_argument_before_span(
    source: &str,
    program: &[SNode],
    span: Span,
    argument: &str,
) -> Option<FixEdit> {
    let carriers = capability_carrier_param_names(program, span);
    let mut edit = None;
    visit::walk_program(program, &mut |node| {
        let Node::FunctionCall { args, .. } = &node.node else {
            return;
        };
        let Some(index) = args.iter().position(|candidate| {
            candidate.span.start == span.start && candidate.span.end == span.end
        }) else {
            return;
        };
        if !args[..index]
            .iter()
            .all(|preceding| argument_carries_a_capability(preceding, &carriers))
        {
            return;
        }
        edit = if index == 0 {
            add_call_argument_edit(source, &node.span, argument)
        } else {
            let previous = args[index - 1].span;
            Some(FixEdit {
                span: Span::with_offsets(
                    previous.end,
                    previous.end,
                    previous.end_line,
                    previous.column,
                ),
                replacement: format!(", {argument}"),
            })
        };
    });
    edit
}

/// Name of the root `Harness` parameter in the narrowest declaration enclosing
/// `span`.
///
/// `capability_argument_for_span` resolves narrow capability handles and can
/// fall back to a root, but it is keyed by `CapabilityId`, so it cannot be asked
/// for the root itself. Replacements that take a whole `Harness` — such as
/// `with_scenario` — need exactly that.
pub(super) fn root_harness_argument_for_span(program: &[SNode], span: Span) -> Option<String> {
    let mut candidates = Vec::new();
    visit::walk_program(program, &mut |node| {
        let params = match &node.node {
            Node::FnDecl { params, .. }
            | Node::ToolDecl { params, .. }
            | Node::Pipeline { params, .. }
                if node.span.start <= span.start && node.span.end >= span.end =>
            {
                params
            }
            _ => return,
        };
        for param in params {
            if matches!(param.type_expr.as_ref(), Some(TypeExpr::Named(name)) if name == "Harness")
            {
                candidates.push((
                    node.span.end.saturating_sub(node.span.start),
                    param.name.clone(),
                ));
                break;
            }
        }
    });
    candidates.sort_by_key(|(width, _)| *width);
    candidates.into_iter().next().map(|(_, name)| name)
}

pub(super) fn capability_argument_for_span(
    program: &[SNode],
    span: Span,
    expected: &str,
) -> Option<String> {
    let capability = harn_builtin_meta::CapabilityId::from_type_name(expected)?;
    let field_name = capability.field_name();
    let mut candidates = Vec::new();
    visit::walk_program(program, &mut |node| {
        let params = match &node.node {
            Node::FnDecl { params, .. }
            | Node::ToolDecl { params, .. }
            | Node::Pipeline { params, .. }
                if node.span.start <= span.start && node.span.end >= span.end =>
            {
                params
            }
            _ => return,
        };
        let mut direct = None;
        let mut bundled = None;
        let mut root = None;
        for param in params {
            match param.type_expr.as_ref() {
                Some(TypeExpr::Named(name)) if name == expected => {
                    direct = Some(param.name.clone());
                }
                Some(TypeExpr::Named(name)) if name == "Harness" => {
                    root = Some(format!("{}.{}", param.name, field_name));
                }
                Some(TypeExpr::Shape(fields))
                    if fields.iter().any(|field| {
                        field.name == field_name
                            && matches!(&field.type_expr, TypeExpr::Named(name) if name == expected)
                    }) =>
                {
                    bundled = Some(format!("{}.{}", param.name, field_name));
                }
                _ => {}
            }
        }
        if let Some(argument) = direct.or(bundled).or(root) {
            candidates.push((node.span.end.saturating_sub(node.span.start), argument));
        }
    });
    candidates.sort_by_key(|(width, _)| *width);
    candidates.into_iter().next().map(|(_, argument)| argument)
}

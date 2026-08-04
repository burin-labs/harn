//! Decide which callables must accept a Harness parameter, and express that
//! decision as source edits.
//!
//! A repair that hands one function a capability is only correct if every
//! caller can supply it, so this walks the callable graph to a fixed point
//! before emitting a single edit.

use std::collections::{BTreeMap, BTreeSet};

use harn_lexer::{FixEdit, Span};
use harn_parser::{
    visit, BindingPattern, DiagnosticCode as Code, Node, Repair, SNode, TypeExpr, TypedParam,
};

use super::capability_migrations::collect_callable_node_calls;
use super::CallableInfo;

pub(super) fn collect_callable_infos(
    program: &[SNode],
    source: &str,
    exported_names: &BTreeSet<String>,
) -> Vec<CallableInfo> {
    let value_referenced = collect_value_referenced_names(program);
    let mut infos = Vec::new();
    for node in program {
        let inner = match &node.node {
            Node::AttributedDecl { inner, .. } => inner.as_ref(),
            _ => node,
        };
        match &inner.node {
            Node::FnDecl {
                name,
                params,
                body,
                is_pub,
                ..
            }
            | Node::ToolDecl {
                name,
                params,
                body,
                is_pub,
                ..
            } => {
                let mut calls = Vec::new();
                let mut ambient_capability_calls = Vec::new();
                visit_callable_body(inner, &mut |child| {
                    collect_callable_node_calls(
                        child,
                        source,
                        &mut calls,
                        &mut ambient_capability_calls,
                    );
                });
                let Some((insert_offset, has_params)) = callable_param_insert(source, inner.span)
                else {
                    continue;
                };
                let bound_names = callable_bound_names(params, body);
                infos.push(CallableInfo {
                    name: name.clone(),
                    span: inner.span,
                    is_exported: *is_pub || exported_names.contains(name),
                    insert_offset,
                    has_params: has_params || !params.is_empty(),
                    bound_names,
                    harness_binding: harness_param_name(params).map(str::to_string),
                    can_change_signature: !value_referenced.contains(name),
                    calls,
                    ambient_capability_calls,
                });
            }
            Node::Pipeline {
                name,
                params,
                body,
                is_pub,
                ..
            } => {
                let mut calls = Vec::new();
                let mut ambient_capability_calls = Vec::new();
                visit_callable_body(inner, &mut |child| {
                    collect_callable_node_calls(
                        child,
                        source,
                        &mut calls,
                        &mut ambient_capability_calls,
                    );
                });
                let Some((insert_offset, has_params)) = callable_param_insert(source, inner.span)
                else {
                    continue;
                };
                let bound_names = callable_bound_names(params, body);
                infos.push(CallableInfo {
                    name: name.clone(),
                    span: inner.span,
                    is_exported: *is_pub || exported_names.contains(name),
                    insert_offset,
                    has_params: has_params || !params.is_empty(),
                    bound_names,
                    harness_binding: harness_param_name(params).map(str::to_string),
                    can_change_signature: !value_referenced.contains(name),
                    calls,
                    ambient_capability_calls,
                });
            }
            _ => {}
        }
    }
    infos
}

/// Names that appear somewhere in the program as a value rather than as the
/// callee of a call.
///
/// A callable referenced as a value carries a function type at that site, so
/// adding a parameter changes its type and breaks the reference — most often a
/// typed parameter default (`resolver: ResolverFn = resolve_thing`), where the
/// declared alias keeps the old arity. A call names its callee in a `String`
/// field, so any bare `Identifier` matching a callable is a value reference.
///
/// A local binding that shadows a top-level callable produces a spurious hit.
/// That direction is safe: it suppresses a repair rather than emitting one that
/// does not compile.
pub(super) fn collect_value_referenced_names(program: &[SNode]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut record = |node: &SNode| {
        if let Node::Identifier(name) = &node.node {
            names.insert(name.clone());
        }
    };
    for node in program {
        visit::walk_node(node, &mut record);
        let inner = match &node.node {
            Node::AttributedDecl { inner, .. } => inner.as_ref(),
            _ => node,
        };
        // `walk_node` reaches fn/tool parameter defaults but not pipeline ones.
        if let Node::Pipeline { params, .. } = &inner.node {
            for param in params {
                if let Some(default) = &param.default_value {
                    visit::walk_node(default, &mut record);
                }
            }
        }
    }
    names
}

fn callable_bound_names(params: &[TypedParam], body: &[SNode]) -> BTreeSet<String> {
    let mut names = params
        .iter()
        .map(|param| param.name.clone())
        .collect::<BTreeSet<_>>();
    collect_binding_names(body, &mut names);
    names
}

fn collect_binding_names(nodes: &[SNode], names: &mut BTreeSet<String>) {
    for node in nodes {
        visit::walk_node(node, &mut |child| match &child.node {
            Node::LetBinding { pattern, .. } | Node::ConstBinding { pattern, .. } => {
                collect_pattern_names(pattern, names);
            }
            Node::ForIn { pattern, .. } => {
                collect_pattern_names(pattern, names);
            }
            Node::Parallel {
                variable: Some(name),
                ..
            } => {
                names.insert(name.clone());
            }
            Node::TryCatch {
                error_var: Some(name),
                ..
            } => {
                names.insert(name.clone());
            }
            Node::Closure { params, .. } => {
                names.extend(params.iter().map(|param| param.name.clone()));
            }
            _ => {}
        });
    }
}

fn collect_pattern_names(pattern: &BindingPattern, names: &mut BTreeSet<String>) {
    match pattern {
        BindingPattern::Identifier(name) => {
            names.insert(name.clone());
        }
        BindingPattern::Dict(fields) => {
            names.extend(
                fields
                    .iter()
                    .map(|field| field.alias.as_ref().unwrap_or(&field.key).clone()),
            );
        }
        BindingPattern::List(elements) => {
            names.extend(elements.iter().map(|element| element.name.clone()));
        }
        BindingPattern::Pair(left, right) => {
            names.insert(left.clone());
            names.insert(right.clone());
        }
    }
}

fn visit_callable_body(node: &SNode, visitor: &mut impl FnMut(&SNode)) {
    let (params, body) = match &node.node {
        Node::FnDecl { params, body, .. }
        | Node::ToolDecl { params, body, .. }
        | Node::Pipeline { params, body, .. } => (params, body),
        _ => return,
    };
    for param in params {
        if let Some(default) = &param.default_value {
            visit::walk_node(default, visitor);
        }
    }
    for stmt in body {
        visit::walk_node(stmt, visitor);
    }
}

pub(super) fn callable_param_insert(source: &str, span: Span) -> Option<(usize, bool)> {
    let region = source.get(span.start..span.end)?;
    let open_paren = region.find('(')?;
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    let mut close_paren = None;
    for (offset, ch) in region[open_paren..].char_indices() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == delimiter {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '"' | '\'') {
            quote = Some(ch);
            continue;
        }
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    close_paren = Some(open_paren + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let close_paren = close_paren?;
    let has_params = !region[open_paren + 1..close_paren].trim().is_empty();
    Some((span.start + open_paren + 1, has_params))
}

fn harness_param_name(params: &[TypedParam]) -> Option<&str> {
    params.iter().find_map(|param| {
        let TypeExpr::Named(name) = param.type_expr.as_ref()? else {
            return None;
        };
        if name == "Harness" && matches!(param.name.as_str(), "harness" | "_harness") {
            Some(param.name.as_str())
        } else {
            None
        }
    })
}

pub(super) fn build_reverse_callers(infos: &[CallableInfo]) -> Vec<Vec<(usize, usize)>> {
    let by_name = infos
        .iter()
        .enumerate()
        .map(|(idx, info)| (info.name.as_str(), idx))
        .collect::<BTreeMap<_, _>>();
    let mut reverse = vec![Vec::new(); infos.len()];
    for (caller_idx, info) in infos.iter().enumerate() {
        for (call_idx, call) in info.calls.iter().enumerate() {
            let Some(&callee_idx) = by_name.get(call.callee.as_str()) else {
                continue;
            };
            reverse[callee_idx].push((caller_idx, call_idx));
        }
    }
    reverse
}

/// The transitive set of callables that must accept a Harness, or `None` when
/// any member cannot.
///
/// Threading is all-or-nothing: a partially threaded set leaves some caller
/// unable to supply the argument, so a set containing one callable that cannot
/// take the parameter means the whole repair is unavailable, not that the
/// callable should be skipped.
pub(super) fn propagate_harness_requirements(
    infos: &[CallableInfo],
    reverse_callers: &[Vec<(usize, usize)>],
    owner_idx: usize,
) -> Option<BTreeSet<usize>> {
    let mut needed = BTreeSet::from([owner_idx]);
    let mut changed = true;
    while changed {
        changed = false;
        let snapshot = needed.iter().copied().collect::<Vec<_>>();
        for callee_idx in snapshot {
            for &(caller_idx, _) in &reverse_callers[callee_idx] {
                if infos[caller_idx].harness_binding.is_none() && needed.insert(caller_idx) {
                    changed = true;
                }
            }
        }
    }
    if needed.iter().any(|&idx| !infos[idx].can_change_signature) {
        return None;
    }
    Some(needed)
}

pub(super) fn repair_for_ambient_capability_plan(
    code: Code,
    infos: &[CallableInfo],
    reverse_callers: &[Vec<(usize, usize)>],
    needed: &BTreeSet<usize>,
) -> Option<Repair> {
    let surface_changing = needed.iter().any(|&idx| {
        let info = &infos[idx];
        info.is_exported || info.name == "main" || reverse_callers[idx].is_empty()
    });
    if surface_changing {
        Some(Repair::from_template(
            Code::InvalidMainSignature.repair_template()?,
        ))
    } else {
        Some(Repair::from_template(code.repair_template()?))
    }
}

pub(super) fn add_harness_param_edit(source: &str, info: &CallableInfo) -> Option<FixEdit> {
    let name = harness_param_name_for_insert(info)?;
    Some(FixEdit {
        span: Span::with_offsets(
            info.insert_offset,
            info.insert_offset,
            info.span.line,
            info.span.column,
        ),
        replacement: prepend_list_item(
            source,
            info.insert_offset,
            &format!("{name}: Harness"),
            info.has_params,
        ),
    })
}

pub(super) fn harness_param_name_for_insert(info: &CallableInfo) -> Option<&'static str> {
    if !info.bound_names.contains("harness") {
        return Some("harness");
    }
    if !info.bound_names.contains("_harness") {
        return Some("_harness");
    }
    None
}

pub(super) fn add_call_argument_edit(source: &str, span: &Span, arg_name: &str) -> Option<FixEdit> {
    let region = source.get(span.start..span.end)?;
    let open_paren = region.find('(')?;
    let close_paren = region[open_paren + 1..].find(')')? + open_paren + 1;
    let has_args = !region[open_paren + 1..close_paren].trim().is_empty();
    let insert_at = span.start + open_paren + 1;
    Some(FixEdit {
        span: Span::with_offsets(insert_at, insert_at, span.line, span.column),
        replacement: prepend_list_item(source, insert_at, arg_name, has_args),
    })
}

/// Render an item inserted immediately after an opening delimiter.
///
/// Multiline lists already own the newline following the delimiter. Adding a
/// horizontal separator before that newline creates trailing whitespace in
/// every migrated declaration or call.
pub(super) fn prepend_list_item(
    source: &str,
    insert_at: usize,
    item: &str,
    has_following_items: bool,
) -> String {
    if !has_following_items {
        return item.to_string();
    }
    let separator = match source.as_bytes().get(insert_at) {
        Some(b'\n' | b'\r') => ",",
        _ => ", ",
    };
    format!("{item}{separator}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_parameter_insertion_has_no_trailing_whitespace() {
        let source = "fn load(\n  path: string,\n) {}\n";
        let span = Span::with_offsets(0, source.len(), 1, 1);
        let (insert_at, has_params) = callable_param_insert(source, span).unwrap();
        let replacement = prepend_list_item(source, insert_at, "harness: HarnessFs", has_params);

        assert_eq!(replacement, "harness: HarnessFs,");
        assert_eq!(
            format!(
                "{}{}{}",
                &source[..insert_at],
                replacement,
                &source[insert_at..]
            ),
            "fn load(harness: HarnessFs,\n  path: string,\n) {}\n"
        );
    }

    #[test]
    fn multiline_call_insertion_has_no_trailing_whitespace() {
        let source = "load(\n  \"config.json\",\n)";
        let span = Span::with_offsets(0, source.len(), 1, 1);
        let edit = add_call_argument_edit(source, &span, "harness.fs").unwrap();

        assert_eq!(edit.replacement, "harness.fs,");
        assert_eq!(
            format!(
                "{}{}{}",
                &source[..edit.span.start],
                edit.replacement,
                &source[edit.span.end..]
            ),
            "load(harness.fs,\n  \"config.json\",\n)"
        );
    }
}

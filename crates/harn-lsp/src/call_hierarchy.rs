use std::collections::{BTreeMap, HashMap};

use harn_lexer::Span;
use harn_parser::{MatchArm, Node, SNode};
use tower_lsp::lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, Position, Range,
    SymbolKind, Url,
};

use crate::document::DocumentState;
use crate::helpers::{span_to_full_range, word_at_position};
use crate::symbols::{HarnSymbolKind, SymbolInfo};

#[derive(Debug, Clone)]
struct CallSite {
    name: String,
    span: Span,
}

#[derive(Debug, Clone)]
struct CallableInfo {
    name: String,
    span: Span,
}

type CallGroupKey = (String, String, u32, u32);
type CallGroup = (CallHierarchyItem, Vec<Range>);

pub(crate) fn prepare_call_hierarchy(
    uri: &Url,
    source: &str,
    symbols: &[SymbolInfo],
    position: Position,
) -> Option<Vec<CallHierarchyItem>> {
    let word = word_at_position(source, position)?;
    let mut candidates = symbols
        .iter()
        .filter(|sym| is_callable_symbol(sym) && sym.name == word)
        .collect::<Vec<_>>();

    if candidates.is_empty() {
        return None;
    }

    candidates.sort_by_key(|sym| {
        let range = span_to_full_range(&sym.def_span, source);
        let contains = position >= range.start && position <= range.end;
        (
            !contains,
            sym.def_span.end.saturating_sub(sym.def_span.start),
        )
    });

    Some(vec![call_hierarchy_item(uri, source, candidates[0])])
}

pub(crate) fn incoming_call_hierarchy(
    item: &CallHierarchyItem,
    docs: &HashMap<Url, DocumentState>,
) -> Option<Vec<CallHierarchyIncomingCall>> {
    let target_name = &item.name;
    let mut grouped: BTreeMap<CallGroupKey, CallGroup> = BTreeMap::new();

    for (uri, state) in docs {
        let Some(program) = state.cached_ast.as_deref() else {
            continue;
        };
        for callable in collect_callables(program) {
            let calls = collect_calls_in_span(program, callable.span)
                .into_iter()
                .filter(|call| call.name == *target_name)
                .map(|call| span_to_full_range(&call.span, &state.source))
                .collect::<Vec<_>>();
            if calls.is_empty() {
                continue;
            }
            let Some(sym) = symbol_for_callable(&state.symbols, &callable, &state.source) else {
                continue;
            };
            let from = call_hierarchy_item(uri, &state.source, sym);
            let key = (
                uri.to_string(),
                from.name.clone(),
                from.range.start.line,
                from.range.start.character,
            );
            grouped
                .entry(key)
                .or_insert_with(|| (from, Vec::new()))
                .1
                .extend(calls);
        }
    }

    let incoming = grouped
        .into_values()
        .map(|(from, from_ranges)| CallHierarchyIncomingCall { from, from_ranges })
        .collect::<Vec<_>>();
    if incoming.is_empty() {
        None
    } else {
        Some(incoming)
    }
}

pub(crate) fn outgoing_call_hierarchy(
    item: &CallHierarchyItem,
    docs: &HashMap<Url, DocumentState>,
) -> Option<Vec<CallHierarchyOutgoingCall>> {
    let state = docs.get(&item.uri)?;
    let program = state.cached_ast.as_deref()?;
    let owner = collect_callables(program).into_iter().find(|callable| {
        callable.name == item.name && span_range_eq(&callable.span, &state.source, item.range)
    })?;

    let mut targets: BTreeMap<CallGroupKey, CallGroup> = BTreeMap::new();

    for call in collect_calls_in_span(program, owner.span) {
        let Some((target_uri, target_state, target_sym)) =
            find_callable_symbol(docs, &call.name, Some(&item.uri))
        else {
            continue;
        };
        let to = call_hierarchy_item(target_uri, &target_state.source, target_sym);
        let key = (
            target_uri.to_string(),
            to.name.clone(),
            to.range.start.line,
            to.range.start.character,
        );
        targets
            .entry(key)
            .or_insert_with(|| (to, Vec::new()))
            .1
            .push(span_to_full_range(&call.span, &state.source));
    }

    let outgoing = targets
        .into_values()
        .map(|(to, from_ranges)| CallHierarchyOutgoingCall { to, from_ranges })
        .collect::<Vec<_>>();
    if outgoing.is_empty() {
        None
    } else {
        Some(outgoing)
    }
}

fn call_hierarchy_item(uri: &Url, source: &str, sym: &SymbolInfo) -> CallHierarchyItem {
    let range = span_to_full_range(&sym.def_span, source);
    CallHierarchyItem {
        name: sym.name.clone(),
        kind: SymbolKind::FUNCTION,
        tags: None,
        detail: sym.signature.clone(),
        uri: uri.clone(),
        range,
        selection_range: range,
        data: None,
    }
}

fn is_callable_symbol(sym: &SymbolInfo) -> bool {
    matches!(
        sym.kind,
        HarnSymbolKind::Function | HarnSymbolKind::Pipeline
    )
}

fn span_range_eq(span: &Span, source: &str, range: Range) -> bool {
    span_to_full_range(span, source) == range
}

fn symbol_for_callable<'a>(
    symbols: &'a [SymbolInfo],
    callable: &CallableInfo,
    source: &str,
) -> Option<&'a SymbolInfo> {
    symbols.iter().find(|sym| {
        is_callable_symbol(sym)
            && sym.name == callable.name
            && span_range_eq(
                &sym.def_span,
                source,
                span_to_full_range(&callable.span, source),
            )
    })
}

fn find_callable_symbol<'a>(
    docs: &'a HashMap<Url, DocumentState>,
    name: &str,
    preferred_uri: Option<&'a Url>,
) -> Option<(&'a Url, &'a DocumentState, &'a SymbolInfo)> {
    if let Some(uri) = preferred_uri {
        if let Some(state) = docs.get(uri) {
            if let Some(sym) = state
                .symbols
                .iter()
                .find(|sym| is_callable_symbol(sym) && sym.name == name)
            {
                return Some((uri, state, sym));
            }
        }
    }

    let mut entries = docs.iter().collect::<Vec<_>>();
    entries.sort_by_key(|(uri, _)| uri.as_str());
    for (uri, state) in entries {
        if let Some(sym) = state
            .symbols
            .iter()
            .find(|sym| is_callable_symbol(sym) && sym.name == name)
        {
            return Some((uri, state, sym));
        }
    }
    None
}

fn collect_callables(program: &[SNode]) -> Vec<CallableInfo> {
    let mut callables = Vec::new();
    for node in program {
        collect_callables_in_node(node, &mut callables);
    }
    callables
}

fn collect_callables_in_node(node: &SNode, callables: &mut Vec<CallableInfo>) {
    match &node.node {
        Node::AttributedDecl { inner, .. } => collect_callables_in_node(inner, callables),
        Node::Pipeline { name, body, .. }
        | Node::FnDecl { name, body, .. }
        | Node::ToolDecl { name, body, .. }
        | Node::OverrideDecl { name, body, .. } => {
            callables.push(CallableInfo {
                name: name.clone(),
                span: node.span,
            });
            for stmt in body {
                collect_callables_in_node(stmt, callables);
            }
        }
        Node::ImplBlock { methods, .. } => {
            for method in methods {
                collect_callables_in_node(method, callables);
            }
        }
        Node::SkillDecl { fields, .. } => {
            for (_, value) in fields {
                collect_callables_in_node(value, callables);
            }
        }
        _ => visit_children_for_callables(node, callables),
    }
}

fn visit_children_for_callables(node: &SNode, callables: &mut Vec<CallableInfo>) {
    match &node.node {
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            collect_callables_in_node(condition, callables);
            for stmt in then_body {
                collect_callables_in_node(stmt, callables);
            }
            if let Some(else_body) = else_body {
                for stmt in else_body {
                    collect_callables_in_node(stmt, callables);
                }
            }
        }
        Node::ForIn { iterable, body, .. } => {
            collect_callables_in_node(iterable, callables);
            for stmt in body {
                collect_callables_in_node(stmt, callables);
            }
        }
        Node::MatchExpr { value, arms } => {
            collect_callables_in_node(value, callables);
            for arm in arms {
                visit_match_arm_for_callables(arm, callables);
            }
        }
        Node::TryCatch {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            for stmt in body.iter().chain(catch_body) {
                collect_callables_in_node(stmt, callables);
            }
            if let Some(finally_body) = finally_body {
                for stmt in finally_body {
                    collect_callables_in_node(stmt, callables);
                }
            }
        }
        Node::Block(stmts)
        | Node::SpawnExpr { body: stmts }
        | Node::WhileLoop { body: stmts, .. }
        | Node::Retry { body: stmts, .. }
        | Node::TryExpr { body: stmts }
        | Node::DeferStmt { body: stmts }
        | Node::DeadlineBlock { body: stmts, .. }
        | Node::MutexBlock { body: stmts }
        | Node::Closure { body: stmts, .. } => {
            for stmt in stmts {
                collect_callables_in_node(stmt, callables);
            }
        }
        Node::Parallel {
            expr,
            body,
            options,
            ..
        } => {
            collect_callables_in_node(expr, callables);
            for stmt in body {
                collect_callables_in_node(stmt, callables);
            }
            for (_, value) in options {
                collect_callables_in_node(value, callables);
            }
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            for case in cases {
                collect_callables_in_node(&case.channel, callables);
                for stmt in &case.body {
                    collect_callables_in_node(stmt, callables);
                }
            }
            if let Some((duration, body)) = timeout {
                collect_callables_in_node(duration, callables);
                for stmt in body {
                    collect_callables_in_node(stmt, callables);
                }
            }
            if let Some(body) = default_body {
                for stmt in body {
                    collect_callables_in_node(stmt, callables);
                }
            }
        }
        _ => {}
    }
}

fn visit_match_arm_for_callables(arm: &MatchArm, callables: &mut Vec<CallableInfo>) {
    collect_callables_in_node(&arm.pattern, callables);
    if let Some(guard) = &arm.guard {
        collect_callables_in_node(guard, callables);
    }
    for stmt in &arm.body {
        collect_callables_in_node(stmt, callables);
    }
}

fn collect_calls_in_span(program: &[SNode], owner_span: Span) -> Vec<CallSite> {
    let mut calls = Vec::new();
    for node in program {
        collect_calls_from_owner(node, owner_span, &mut calls);
    }
    calls
}

fn collect_calls_from_owner(node: &SNode, owner_span: Span, calls: &mut Vec<CallSite>) -> bool {
    match &node.node {
        Node::AttributedDecl { inner, .. } => collect_calls_from_owner(inner, owner_span, calls),
        Node::Pipeline { body, .. }
        | Node::FnDecl { body, .. }
        | Node::ToolDecl { body, .. }
        | Node::OverrideDecl { body, .. }
            if node.span == owner_span =>
        {
            collect_calls_in_body(body, calls);
            true
        }
        Node::ImplBlock { methods, .. } => {
            for method in methods {
                if collect_calls_from_owner(method, owner_span, calls) {
                    return true;
                }
            }
            false
        }
        _ => visit_child_owners(node, owner_span, calls),
    }
}

fn visit_child_owners(node: &SNode, owner_span: Span, calls: &mut Vec<CallSite>) -> bool {
    match &node.node {
        Node::IfElse {
            then_body,
            else_body,
            ..
        } => {
            body_contains_owner(then_body, owner_span, calls)
                || else_body
                    .as_ref()
                    .is_some_and(|body| body_contains_owner(body, owner_span, calls))
        }
        Node::ForIn { body, .. }
        | Node::Block(body)
        | Node::SpawnExpr { body }
        | Node::WhileLoop { body, .. }
        | Node::Retry { body, .. }
        | Node::TryExpr { body }
        | Node::DeferStmt { body }
        | Node::DeadlineBlock { body, .. }
        | Node::MutexBlock { body }
        | Node::Closure { body, .. } => body_contains_owner(body, owner_span, calls),
        Node::TryCatch {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            body_contains_owner(body, owner_span, calls)
                || body_contains_owner(catch_body, owner_span, calls)
                || finally_body
                    .as_ref()
                    .is_some_and(|body| body_contains_owner(body, owner_span, calls))
        }
        Node::Parallel { body, .. } => body_contains_owner(body, owner_span, calls),
        Node::MatchExpr { arms, .. } => arms
            .iter()
            .any(|arm| body_contains_owner(&arm.body, owner_span, calls)),
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            cases
                .iter()
                .any(|case| body_contains_owner(&case.body, owner_span, calls))
                || timeout
                    .as_ref()
                    .is_some_and(|(_, body)| body_contains_owner(body, owner_span, calls))
                || default_body
                    .as_ref()
                    .is_some_and(|body| body_contains_owner(body, owner_span, calls))
        }
        Node::GuardStmt { else_body, .. } => body_contains_owner(else_body, owner_span, calls),
        Node::SkillDecl { fields, .. } => fields
            .iter()
            .any(|(_, value)| collect_calls_from_owner(value, owner_span, calls)),
        _ => false,
    }
}

fn body_contains_owner(body: &[SNode], owner_span: Span, calls: &mut Vec<CallSite>) -> bool {
    for stmt in body {
        if collect_calls_from_owner(stmt, owner_span, calls) {
            return true;
        }
    }
    false
}

fn collect_calls_in_body(body: &[SNode], calls: &mut Vec<CallSite>) {
    for stmt in body {
        collect_calls(stmt, calls);
    }
}

fn collect_calls(node: &SNode, calls: &mut Vec<CallSite>) {
    match &node.node {
        Node::FunctionCall { name, args, .. } => {
            calls.push(CallSite {
                name: name.clone(),
                span: node.span,
            });
            for arg in args {
                collect_calls(arg, calls);
            }
        }
        Node::MethodCall {
            object,
            method,
            args,
        }
        | Node::OptionalMethodCall {
            object,
            method,
            args,
        } => {
            collect_calls(object, calls);
            calls.push(CallSite {
                name: method.clone(),
                span: node.span,
            });
            for arg in args {
                collect_calls(arg, calls);
            }
        }
        Node::AttributedDecl { inner, .. } => collect_calls(inner, calls),
        Node::Pipeline { body, .. }
        | Node::FnDecl { body, .. }
        | Node::ToolDecl { body, .. }
        | Node::OverrideDecl { body, .. } => collect_calls_in_body(body, calls),
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            collect_calls(condition, calls);
            collect_calls_in_body(then_body, calls);
            if let Some(else_body) = else_body {
                collect_calls_in_body(else_body, calls);
            }
        }
        Node::ForIn { iterable, body, .. } => {
            collect_calls(iterable, calls);
            collect_calls_in_body(body, calls);
        }
        Node::MatchExpr { value, arms } => {
            collect_calls(value, calls);
            for arm in arms {
                collect_calls(&arm.pattern, calls);
                if let Some(guard) = &arm.guard {
                    collect_calls(guard, calls);
                }
                collect_calls_in_body(&arm.body, calls);
            }
        }
        Node::TryCatch {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            collect_calls_in_body(body, calls);
            collect_calls_in_body(catch_body, calls);
            if let Some(finally_body) = finally_body {
                collect_calls_in_body(finally_body, calls);
            }
        }
        Node::Block(stmts)
        | Node::SpawnExpr { body: stmts }
        | Node::WhileLoop { body: stmts, .. }
        | Node::Retry { body: stmts, .. }
        | Node::TryExpr { body: stmts }
        | Node::DeferStmt { body: stmts }
        | Node::DeadlineBlock { body: stmts, .. }
        | Node::MutexBlock { body: stmts }
        | Node::Closure { body: stmts, .. } => collect_calls_in_body(stmts, calls),
        Node::Parallel {
            expr,
            body,
            options,
            ..
        } => {
            collect_calls(expr, calls);
            collect_calls_in_body(body, calls);
            for (_, value) in options {
                collect_calls(value, calls);
            }
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            for case in cases {
                collect_calls(&case.channel, calls);
                collect_calls_in_body(&case.body, calls);
            }
            if let Some((duration, body)) = timeout {
                collect_calls(duration, calls);
                collect_calls_in_body(body, calls);
            }
            if let Some(body) = default_body {
                collect_calls_in_body(body, calls);
            }
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            collect_calls(condition, calls);
            collect_calls_in_body(else_body, calls);
        }
        Node::RequireStmt { condition, message } => {
            collect_calls(condition, calls);
            if let Some(message) = message {
                collect_calls(message, calls);
            }
        }
        Node::LetBinding { value, .. }
        | Node::VarBinding { value, .. }
        | Node::ThrowStmt { value }
        | Node::EmitExpr { value } => collect_calls(value, calls),
        Node::ReturnStmt { value } | Node::YieldExpr { value } => {
            if let Some(value) = value {
                collect_calls(value, calls);
            }
        }
        Node::BinaryOp { left, right, .. }
        | Node::RangeExpr {
            start: left,
            end: right,
            ..
        } => {
            collect_calls(left, calls);
            collect_calls(right, calls);
        }
        Node::UnaryOp { operand, .. }
        | Node::TryOperator { operand }
        | Node::TryStar { operand }
        | Node::Spread(operand) => collect_calls(operand, calls),
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            collect_calls(condition, calls);
            collect_calls(true_expr, calls);
            collect_calls(false_expr, calls);
        }
        Node::Assignment { target, value, .. } => {
            collect_calls(target, calls);
            collect_calls(value, calls);
        }
        Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
            collect_calls(object, calls);
        }
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            collect_calls(object, calls);
            collect_calls(index, calls);
        }
        Node::SliceAccess { object, start, end } => {
            collect_calls(object, calls);
            if let Some(start) = start {
                collect_calls(start, calls);
            }
            if let Some(end) = end {
                collect_calls(end, calls);
            }
        }
        Node::ListLiteral(items) | Node::OrPattern(items) => {
            for item in items {
                collect_calls(item, calls);
            }
        }
        Node::DictLiteral(entries)
        | Node::StructConstruct {
            fields: entries, ..
        } => {
            for entry in entries {
                collect_calls(&entry.key, calls);
                collect_calls(&entry.value, calls);
            }
        }
        Node::EnumConstruct { args, .. } => {
            for arg in args {
                collect_calls(arg, calls);
            }
        }
        Node::ImplBlock { methods, .. } => collect_calls_in_body(methods, calls),
        Node::SkillDecl { fields, .. } => {
            for (_, value) in fields {
                collect_calls(value, calls);
            }
        }
        Node::ImportDecl { .. }
        | Node::SelectiveImport { .. }
        | Node::EnumDecl { .. }
        | Node::StructDecl { .. }
        | Node::InterfaceDecl { .. }
        | Node::TypeDecl { .. }
        | Node::DurationLiteral(_)
        | Node::StringLiteral(_)
        | Node::RawStringLiteral(_)
        | Node::InterpolatedString(_)
        | Node::IntLiteral(_)
        | Node::FloatLiteral(_)
        | Node::BoolLiteral(_)
        | Node::NilLiteral
        | Node::Identifier(_)
        | Node::BreakStmt
        | Node::ContinueStmt => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{incoming_call_hierarchy, outgoing_call_hierarchy, prepare_call_hierarchy};
    use crate::document::DocumentState;
    use std::collections::HashMap;
    use tower_lsp::lsp_types::{Position, Url};

    #[test]
    fn prepares_and_resolves_incoming_and_outgoing_calls() {
        let source = concat!(
            "fn callee(value) {\n",
            "  return value\n",
            "}\n",
            "\n",
            "fn helper(value) {\n",
            "  return callee(value)\n",
            "}\n",
            "\n",
            "pipeline main(task) {\n",
            "  helper(task)\n",
            "  callee(task)\n",
            "}\n",
        );
        let uri = Url::parse("file:///test.harn").unwrap();
        let state = DocumentState::new(source.to_string());

        let prepared = prepare_call_hierarchy(&uri, source, &state.symbols, Position::new(0, 4))
            .expect("callee should prepare");
        assert_eq!(prepared[0].name, "callee");

        let mut docs = HashMap::new();
        docs.insert(uri.clone(), state);

        let incoming = incoming_call_hierarchy(&prepared[0], &docs).expect("incoming calls");
        let incoming_names = incoming
            .iter()
            .map(|call| call.from.name.as_str())
            .collect::<Vec<_>>();
        assert!(incoming_names.contains(&"helper"), "{incoming:?}");
        assert!(incoming_names.contains(&"main"), "{incoming:?}");

        let main_item = prepare_call_hierarchy(
            &uri,
            source,
            &docs.get(&uri).unwrap().symbols,
            Position::new(8, 10),
        )
        .expect("main should prepare")
        .remove(0);
        let outgoing = outgoing_call_hierarchy(&main_item, &docs).expect("outgoing calls");
        let outgoing_names = outgoing
            .iter()
            .map(|call| call.to.name.as_str())
            .collect::<Vec<_>>();
        assert!(outgoing_names.contains(&"helper"), "{outgoing:?}");
        assert!(outgoing_names.contains(&"callee"), "{outgoing:?}");
    }
}

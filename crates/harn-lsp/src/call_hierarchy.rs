use std::collections::{BTreeMap, HashMap};

use harn_lexer::Span;
use harn_modules::DefSite;
use harn_parser::{Node, SNode};
use tower_lsp::lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, Position, Range,
    SymbolKind, Url,
};

use crate::document::DocumentState;
use crate::helpers::{span_to_full_range, word_at_position};
use crate::resolution::WorkspaceRefs;
use crate::source_text::SourceText;
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
    source: &SourceText,
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
    workspace: Option<&WorkspaceRefs>,
) -> Option<Vec<CallHierarchyIncomingCall>> {
    let target_def = item_definition(item, workspace);
    let mut grouped: BTreeMap<CallGroupKey, CallGroup> = BTreeMap::new();

    for (uri, state) in docs {
        let Some(program) = state.cached_ast.as_deref() else {
            continue;
        };
        let file = uri.to_file_path().ok();
        for callable in collect_callables(program) {
            let calls = collect_calls_in_span(program, callable.span)
                .into_iter()
                .filter(|call| {
                    call_resolves_to(
                        workspace,
                        file.as_deref(),
                        call,
                        target_def.as_ref(),
                        &item.name,
                    )
                })
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
    workspace: Option<&WorkspaceRefs>,
) -> Option<Vec<CallHierarchyOutgoingCall>> {
    let state = docs.get(&item.uri)?;
    let program = state.cached_ast.as_deref()?;
    let owner = collect_callables(program).into_iter().find(|callable| {
        callable.name == item.name && span_range_eq(&callable.span, &state.source, item.range)
    })?;

    let mut targets: BTreeMap<CallGroupKey, CallGroup> = BTreeMap::new();

    for call in collect_calls_in_span(program, owner.span) {
        let file = item.uri.to_file_path().ok();
        let resolved = file
            .as_deref()
            .and_then(|path| workspace.and_then(|ws| ws.graph.definition_of(path, &call.name)));
        let Some((target_uri, target_state, target_sym)) = resolved
            .as_ref()
            .and_then(|def| find_callable_at(docs, def))
            .or_else(|| find_callable_symbol(docs, &call.name, Some(&item.uri)))
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

fn call_hierarchy_item(uri: &Url, source: &SourceText, sym: &SymbolInfo) -> CallHierarchyItem {
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

fn span_range_eq(span: &Span, source: &SourceText, range: Range) -> bool {
    span_to_full_range(span, source) == range
}

fn symbol_for_callable<'a>(
    symbols: &'a [SymbolInfo],
    callable: &CallableInfo,
    source: &SourceText,
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

fn item_definition(item: &CallHierarchyItem, workspace: Option<&WorkspaceRefs>) -> Option<DefSite> {
    let path = item.uri.to_file_path().ok()?;
    workspace?.graph.definition_of(&path, &item.name)
}

fn call_resolves_to(
    workspace: Option<&WorkspaceRefs>,
    file: Option<&std::path::Path>,
    call: &CallSite,
    target_def: Option<&DefSite>,
    fallback_name: &str,
) -> bool {
    match (workspace, file, target_def) {
        (Some(ws), Some(path), Some(target)) => {
            ws.graph.definition_of(path, &call.name).is_some_and(|def| {
                def.file == target.file && def.name == target.name && def.span == target.span
            })
        }
        _ => call.name == fallback_name,
    }
}

fn find_callable_at<'a>(
    docs: &'a HashMap<Url, DocumentState>,
    def: &DefSite,
) -> Option<(&'a Url, &'a DocumentState, &'a SymbolInfo)> {
    for (uri, state) in docs {
        let Ok(path) = uri.to_file_path() else {
            continue;
        };
        if harn_modules::canonical_path(&path) != def.file {
            continue;
        }
        if let Some(sym) = state
            .symbols
            .iter()
            .find(|sym| is_callable_symbol(sym) && sym.name == def.name)
        {
            return Some((uri, state, sym));
        }
    }
    None
}

fn collect_callables(program: &[SNode]) -> Vec<CallableInfo> {
    let mut callables = Vec::new();
    harn_parser::visit::walk_program(program, &mut |node| {
        if let Node::Pipeline { name, .. }
        | Node::FnDecl { name, .. }
        | Node::ToolDecl { name, .. }
        | Node::OverrideDecl { name, .. } = &node.node
        {
            callables.push(CallableInfo {
                name: name.clone(),
                span: node.span,
            });
        }
    });
    callables
}

fn collect_calls_in_span(program: &[SNode], owner_span: Span) -> Vec<CallSite> {
    let mut body = None;
    harn_parser::visit::walk_program(program, &mut |node| {
        if node.span == owner_span {
            if let Node::Pipeline { body: found, .. }
            | Node::FnDecl { body: found, .. }
            | Node::ToolDecl { body: found, .. }
            | Node::OverrideDecl { body: found, .. } = &node.node
            {
                body = Some(found);
            }
        }
    });
    let mut calls = Vec::new();
    if let Some(body) = body {
        for node in body {
            collect_calls(node, &mut calls);
        }
    }
    calls
}

fn collect_calls(node: &SNode, calls: &mut Vec<CallSite>) {
    match &node.node {
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
            // Preserve receiver-before-method ordering for chained calls.
            collect_calls(object, calls);
            calls.push(CallSite {
                name: method.clone(),
                span: node.span,
            });
            for argument in args {
                collect_calls(argument, calls);
            }
        }
        _ => {
            if let Node::FunctionCall { name, .. } = &node.node {
                calls.push(CallSite {
                    name: name.clone(),
                    span: node.span,
                });
            }
            harn_parser::visit::for_each_immediate_child(node, &mut |child| {
                collect_calls(child, calls);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{incoming_call_hierarchy, outgoing_call_hierarchy, prepare_call_hierarchy};
    use crate::document::DocumentState;
    use std::collections::HashMap;
    use tower_lsp::lsp_types::{Position, Url};

    #[test]
    fn finds_call_owners_declared_inside_bound_closures() {
        let source = "fn leaf() { return 1 }\nfn owner() {\n  const block = { ->\n    fn nested() { return leaf() }\n    nested()\n  }\n  block()\n}\n";
        let uri = Url::parse("file:///nested.harn").unwrap();
        let state = DocumentState::new(source.to_owned());
        let nested =
            prepare_call_hierarchy(&uri, &state.source, &state.symbols, Position::new(3, 8))
                .expect("nested callable prepares")
                .remove(0);
        assert_eq!(nested.name, "nested");
        let leaf = prepare_call_hierarchy(&uri, &state.source, &state.symbols, Position::new(0, 4))
            .expect("leaf callable prepares")
            .remove(0);
        let docs = HashMap::from([(uri, state)]);
        let outgoing =
            outgoing_call_hierarchy(&nested, &docs, None).expect("nested outgoing calls");
        assert_eq!(outgoing.len(), 1, "{outgoing:?}");
        assert_eq!(outgoing[0].to.name, "leaf");
        let incoming = incoming_call_hierarchy(&leaf, &docs, None).expect("leaf incoming calls");
        assert!(
            incoming.iter().any(|call| call.from.name == "nested"),
            "{incoming:?}"
        );
    }

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

        let prepared =
            prepare_call_hierarchy(&uri, &state.source, &state.symbols, Position::new(0, 4))
                .expect("callee should prepare");
        assert_eq!(prepared[0].name, "callee");

        let mut docs = HashMap::new();
        docs.insert(uri.clone(), state);

        let incoming = incoming_call_hierarchy(&prepared[0], &docs, None).expect("incoming calls");
        let incoming_names = incoming
            .iter()
            .map(|call| call.from.name.as_str())
            .collect::<Vec<_>>();
        assert!(incoming_names.contains(&"helper"), "{incoming:?}");
        assert!(incoming_names.contains(&"main"), "{incoming:?}");

        let main_item = prepare_call_hierarchy(
            &uri,
            &docs.get(&uri).unwrap().source,
            &docs.get(&uri).unwrap().symbols,
            Position::new(8, 10),
        )
        .expect("main should prepare")
        .remove(0);
        let outgoing = outgoing_call_hierarchy(&main_item, &docs, None).expect("outgoing calls");
        let outgoing_names = outgoing
            .iter()
            .map(|call| call.to.name.as_str())
            .collect::<Vec<_>>();
        assert!(outgoing_names.contains(&"helper"), "{outgoing:?}");
        assert!(outgoing_names.contains(&"callee"), "{outgoing:?}");
    }
}

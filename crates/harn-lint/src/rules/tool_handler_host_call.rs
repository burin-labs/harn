//! Reject privileged host-wire reads reachable from a tool handler.

use std::collections::{BTreeMap, BTreeSet};

use harn_parser::visit;
use harn_parser::{DiagnosticCode as Code, DictEntry, Node, SNode};

use crate::diagnostic::{LintDiagnostic, LintSeverity};

const RULE_NAME: &str = "tool-handler-host-call";

pub(crate) fn check_tool_handler_host_call(
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let mut functions = BTreeMap::new();
    for node in program {
        record_function(node, &mut functions);
    }

    let mut hits = Vec::new();
    visit::walk_program(program, &mut |node| match &node.node {
        Node::ToolDecl { body, .. } => {
            collect_reachable_host_calls(body, &functions, &mut BTreeSet::new(), &mut hits);
        }
        Node::DictLiteral(entries) => {
            if let Some(body) = handler_body(entries) {
                collect_reachable_host_calls(body, &functions, &mut BTreeSet::new(), &mut hits);
            }
        }
        _ => {}
    });

    hits.sort_by_key(|span| (span.start, span.end));
    hits.dedup_by_key(|span| (span.start, span.end));
    diagnostics.extend(hits.into_iter().map(|span| LintDiagnostic {
        code: Code::LintToolHandlerHostCall,
        rule: RULE_NAME.into(),
        message: "a tool handler reaches the privileged `host_call` wire, which is not serviceable from a model- or client-invoked handler".to_string(),
        span,
        severity: LintSeverity::Warning,
        suggestion: Some(
            "read the host-owned value at the trusted entry boundary, then close over it or pass a typed capability into the handler".to_string(),
        ),
        fix: None,
    }));
}

fn record_function<'a>(node: &'a SNode, functions: &mut BTreeMap<&'a str, &'a [SNode]>) {
    match &node.node {
        Node::FnDecl { name, body, .. } => {
            functions.insert(name, body);
        }
        Node::AttributedDecl { inner, .. } => record_function(inner, functions),
        _ => {}
    }
}

fn handler_body(entries: &[DictEntry]) -> Option<&[SNode]> {
    let handler = entries.iter().find(|entry| {
        matches!(
            &entry.key.node,
            Node::Identifier(key) | Node::StringLiteral(key) | Node::RawStringLiteral(key)
                if key == "handler"
        )
    })?;
    match &handler.value.node {
        Node::Closure { body, .. } => Some(body),
        _ => None,
    }
}

fn collect_reachable_host_calls(
    body: &[SNode],
    functions: &BTreeMap<&str, &[SNode]>,
    visited: &mut BTreeSet<String>,
    hits: &mut Vec<harn_lexer::Span>,
) {
    let mut calls = Vec::new();
    visit::walk_program(body, &mut |node| {
        if let Node::FunctionCall { name, .. } = &node.node {
            calls.push((name.clone(), node.span));
        }
    });
    for (name, span) in calls {
        if name == "host_call" {
            hits.push(span);
        } else if visited.insert(name.clone()) {
            if let Some(callee) = functions.get(name.as_str()) {
                collect_reachable_host_calls(callee, functions, visited, hits);
            }
        }
    }
}

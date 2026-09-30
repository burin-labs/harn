//! `untyped-tool-handler-result` rule: flag a tool handler that returns a
//! freeform dict literal.
//!
//! A handler's return value declares whether the operation succeeded. When it
//! is a plain dict, nothing in the value says so, and every reader has to infer
//! it from key names. That inference is not completable: a dict carrying
//! `status` may be declaring a failure or merely reporting progress, and no set
//! of key names separates the two. It has already cost one silent defect, where
//! a dict-shaped refusal was reported a success (harn#7884).
//!
//! Error severity for the result shapes this rule can prove. Returns from
//! helpers and mutable locals still need type analysis. Other `handler` keys,
//! such as tool-search strategies, do not declare tool outcomes.

use harn_lexer::Span;
use harn_parser::visit;
use harn_parser::{BindingPattern, DiagnosticCode as Code, DictEntry, Node, SNode};
use harn_vm::llm::AGENT_TOOL_HANDLER_RESULT_SCHEMA;
use std::collections::BTreeSet;

use crate::diagnostic::{LintDiagnostic, LintSeverity};

const RULE_NAME: &str = "untyped-tool-handler-result";

/// Keys whose presence means the dict is already trying to declare an outcome
/// by convention. Those are the returns this rule most wants typed, but a dict
/// without them is no better off — it declares nothing at all.
const CONVENTIONAL_OUTCOME_KEYS: &[&str] = &["ok", "success", "isError", "status", "error"];

pub(crate) fn check_untyped_tool_handler_result(
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let mut strategy_handlers = BTreeSet::new();
    visit::walk_program(program, &mut |node| {
        let Node::DictLiteral(entries) = &node.node else {
            return;
        };
        let Some(search) = entry_for_key(entries, "tool_search") else {
            return;
        };
        let Node::DictLiteral(search_entries) = &search.value.node else {
            return;
        };
        let Some(strategy) = entry_for_key(search_entries, "strategy") else {
            return;
        };
        let Node::DictLiteral(strategy_entries) = &strategy.value.node else {
            return;
        };
        if let Some(handler) = entry_for_key(strategy_entries, "handler") {
            strategy_handlers.insert((handler.value.span.start, handler.value.span.end));
        }
    });
    visit::walk_program(program, &mut |node| {
        let Node::DictLiteral(entries) = &node.node else {
            return;
        };
        let Some(handler) = entry_for_key(entries, "handler") else {
            return;
        };
        if strategy_handlers.contains(&(handler.value.span.start, handler.value.span.end)) {
            return;
        }
        let Node::Closure { body, .. } = &handler.value.node else {
            return;
        };
        for returned in returned_dict_literals(body) {
            diagnostics.push(make_diagnostic(returned));
        }
    });
}

/// Every dict literal this closure body can hand back: an explicit `return`,
/// or a trailing expression in tail position.
///
/// An immutable local initialized directly from a dict literal is also known
/// without type inference. Helpers and mutable locals still need type analysis;
/// the rule does not guess about their results.
///
/// The typed result envelope is a dict literal too, and it is the shape this
/// rule's own suggestion recommends for a text result, so reporting it would
/// make the rule contradict itself. It is excluded by its `schema` key rather
/// than by its other keys: that key is what the runtime reads to decide the
/// value is an envelope, so the lint and the runtime agree by construction.
fn returned_dict_literals(body: &[SNode]) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut untyped_consts = BTreeSet::new();
    for statement in body {
        if let Node::ConstBinding {
            pattern: BindingPattern::Identifier(name),
            value,
            ..
        } = &statement.node
        {
            if let Node::DictLiteral(entries) = &value.node {
                if !is_handler_result_envelope(entries) {
                    untyped_consts.insert(name.as_str());
                }
            }
        }
        if let Node::ReturnStmt { value: Some(value) } = &statement.node {
            if let Node::Identifier(name) = &value.node {
                if untyped_consts.contains(name.as_str()) {
                    spans.push(value.span);
                }
            }
        }
        visit::walk_node(statement, &mut |node| {
            if let Node::ReturnStmt { value: Some(value) } = &node.node {
                if let Node::DictLiteral(entries) = &value.node {
                    if !is_handler_result_envelope(entries) {
                        spans.push(value.span);
                    }
                }
            }
        });
    }
    if let Some(last) = body.last() {
        if let Node::DictLiteral(entries) = &last.node {
            if !is_handler_result_envelope(entries) {
                spans.push(last.span);
            }
        } else if let Node::Identifier(name) = &last.node {
            if untyped_consts.contains(name.as_str()) {
                spans.push(last.span);
            }
        }
    }
    spans.sort_by_key(|span| (span.start, span.end));
    spans.dedup_by_key(|span| (span.start, span.end));
    spans
}

/// Whether this dict declares itself the typed handler-result envelope, by
/// carrying the exact `schema` string the runtime matches on.
///
/// A computed `schema` value does not qualify. The rule cannot evaluate it, and
/// treating an unreadable value as an envelope would silence the warning on
/// every dict that merely mentions the key.
fn is_handler_result_envelope(entries: &[DictEntry]) -> bool {
    entry_for_key(entries, "schema").is_some_and(|entry| {
        matches!(
            &entry.value.node,
            Node::StringLiteral(value) | Node::RawStringLiteral(value)
                if value == AGENT_TOOL_HANDLER_RESULT_SCHEMA
        )
    })
}

fn entry_for_key<'a>(entries: &'a [DictEntry], key: &str) -> Option<&'a DictEntry> {
    entries
        .iter()
        .find(|entry| key_name(&entry.key).as_deref() == Some(key))
}

fn key_name(node: &SNode) -> Option<String> {
    match &node.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) | Node::Identifier(value) => {
            Some(value.clone())
        }
        _ => None,
    }
}

fn make_diagnostic(span: Span) -> LintDiagnostic {
    LintDiagnostic {
        code: Code::LintUntypedToolHandlerResult,
        rule: RULE_NAME.into(),
        message: format!(
            "this tool handler returns a freeform dict, so whether the operation succeeded has to be \
             inferred from key names ({}) rather than declared by the value's type.",
            CONVENTIONAL_OUTCOME_KEYS.join("`, `")
        ),
        span,
        severity: LintSeverity::Error,
        suggestion: Some(
            "return a typed struct whose type declares the outcome, or the \
             `harn.agent_tool_handler_result.v1` envelope for a text result."
                .to_string(),
        ),
        fix: None,
    }
}

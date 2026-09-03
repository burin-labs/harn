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
//! Warning severity while in-tree handlers migrate (harn#7901). It becomes an
//! error once no untyped handler result remains.

use harn_lexer::Span;
use harn_parser::visit;
use harn_parser::{DiagnosticCode as Code, DictEntry, Node, SNode};
use harn_vm::llm::AGENT_TOOL_HANDLER_RESULT_SCHEMA;

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
    visit::walk_program(program, &mut |node| {
        let Node::DictLiteral(entries) = &node.node else {
            return;
        };
        let Some(handler) = entry_for_key(entries, "handler") else {
            return;
        };
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
/// Deliberately shallow. A dict built up in a local and returned by name is not
/// reported, because the rule would then need type inference to say anything
/// true, and a warning that stays quiet on a value it cannot see is better than
/// one that guesses.
///
/// The typed result envelope is a dict literal too, and it is the shape this
/// rule's own suggestion recommends for a text result, so reporting it would
/// make the rule contradict itself. It is excluded by its `schema` key rather
/// than by its other keys: that key is what the runtime reads to decide the
/// value is an envelope, so the lint and the runtime agree by construction.
fn returned_dict_literals(body: &[SNode]) -> Vec<Span> {
    let mut spans = Vec::new();
    for statement in body {
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
        severity: LintSeverity::Warning,
        suggestion: Some(
            "return a typed struct whose type declares the outcome, or the \
             `harn.agent_tool_handler_result.v1` envelope for a text result."
                .to_string(),
        ),
        fix: None,
    }
}

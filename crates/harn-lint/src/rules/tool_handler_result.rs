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
/// true, and a warning that fires on a value it cannot see is worse than one
/// that stays quiet.
fn returned_dict_literals(body: &[SNode]) -> Vec<Span> {
    let mut spans = Vec::new();
    for statement in body {
        visit::walk_node(statement, &mut |node| {
            if let Node::Return(Some(value)) = &node.node {
                if let Node::DictLiteral(_) = &value.node {
                    spans.push(value.span);
                }
            }
        });
    }
    if let Some(last) = body.last() {
        if let Node::DictLiteral(_) = &last.node {
            spans.push(last.span);
        }
    }
    spans.sort_by_key(|span| (span.start, span.end));
    spans.dedup_by_key(|span| (span.start, span.end));
    spans
}

fn entry_for_key<'a>(entries: &'a [DictEntry], key: &str) -> Option<&'a DictEntry> {
    entries.iter().find(|entry| entry.key == key)
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

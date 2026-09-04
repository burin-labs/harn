//! `schema-shaped-tool-parameters` rule: flag a tool descriptor whose
//! `parameters` map is really a JSON Schema document.
//!
//! One key named `parameters` means two things depending on which function
//! receives the descriptor. `tool_define` reads every key as a parameter name;
//! the composition and agent-loop descriptor paths read the same key as a
//! complete schema. A descriptor written for one and handed to the other is
//! wrong in both directions, and the silent direction is the dangerous one:
//! `{type: "object", properties: {}}` under the registry's rule is a
//! no-parameter tool that quietly declares two parameters named `type` and
//! `properties`, which is how a consumer shipped one (harn#7981).
//!
//! The runtime refuses the shape when a registry is built. This rule reports it
//! at check time, including in descriptor literals whose consumer never applies
//! the registry's rule, so the spelling cannot drift back unnoticed.

use harn_lexer::Span;
use harn_parser::visit;
use harn_parser::{DiagnosticCode as Code, DictEntry, Node, SNode};

use crate::diagnostic::{LintDiagnostic, LintSeverity};

const RULE_NAME: &str = "schema-shaped-tool-parameters";

/// Top-level keys that only ever appear together in a JSON Schema document.
/// A per-parameter map drawn entirely from this set is describing a schema.
const JSON_SCHEMA_DOCUMENT_KEYS: &[&str] = &["type", "properties", "required"];

pub(crate) fn check_schema_shaped_tool_parameters(
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    visit::walk_program(program, &mut |node| {
        let Node::DictLiteral(entries) = &node.node else {
            return;
        };
        let Some(parameters) = entry_for_key(entries, "parameters") else {
            return;
        };
        let Node::DictLiteral(inner) = &parameters.value.node else {
            return;
        };
        if schema_shaped(inner) {
            diagnostics.push(make_diagnostic(parameters.value.span));
        }
    });
}

/// Whether every readable top-level key is a JSON Schema document keyword.
///
/// An empty map declares no parameters and is not a schema. A key this rule
/// cannot read is treated as evidence against the schema reading, so a computed
/// parameter name never trips it.
fn schema_shaped(entries: &[DictEntry]) -> bool {
    !entries.is_empty()
        && entries.iter().all(|entry| {
            key_name(&entry.key)
                .is_some_and(|key| JSON_SCHEMA_DOCUMENT_KEYS.contains(&key.as_str()))
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
        code: Code::LintSchemaShapedToolParameters,
        rule: RULE_NAME.into(),
        message: format!(
            "this `parameters` map has only JSON Schema document keys (`{}`), so a reader that \
             treats `parameters` as a per-parameter map sees parameters with those names.",
            JSON_SCHEMA_DOCUMENT_KEYS.join("`, `")
        ),
        span,
        severity: LintSeverity::Error,
        suggestion: Some(
            "spell a complete schema as `inputSchema`, and keep `parameters` for the \
             per-parameter map."
                .to_string(),
        ),
        fix: None,
    }
}

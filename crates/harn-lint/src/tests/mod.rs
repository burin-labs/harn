//! Shared test helpers for the lint crate. Rule-family test files live
//! as sibling submodules and pull helpers in via `use super::*;`.

pub(super) use crate::*;
pub(super) use harn_lexer::{FixEdit, Lexer};
pub(super) use harn_parser::Parser;
pub(super) use std::collections::HashSet;

pub(super) fn lint_source(source: &str) -> Vec<LintDiagnostic> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    lint_with_source(&program, source)
}

pub(super) fn has_rule(diagnostics: &[LintDiagnostic], rule: &str) -> bool {
    diagnostics.iter().any(|d| d.rule == rule)
}

pub(super) fn count_rule(diagnostics: &[LintDiagnostic], rule: &str) -> usize {
    diagnostics.iter().filter(|d| d.rule == rule).count()
}

pub(super) fn get_fix(diagnostics: &[LintDiagnostic], rule: &str) -> Option<Vec<FixEdit>> {
    diagnostics
        .iter()
        .find(|d| d.rule == rule)
        .and_then(|d| d.fix.clone())
}

pub(super) fn apply_fixes(source: &str, diagnostics: &[LintDiagnostic]) -> String {
    let mut edits: Vec<&FixEdit> = diagnostics
        .iter()
        .filter_map(|d| d.fix.as_ref())
        .flatten()
        .collect();
    edits.sort_by_key(|edit| std::cmp::Reverse(edit.span.start));
    let mut accepted: Vec<&FixEdit> = Vec::new();
    for edit in &edits {
        let overlaps = accepted
            .iter()
            .any(|prev| edit.span.start < prev.span.end && edit.span.end > prev.span.start);
        if !overlaps {
            accepted.push(edit);
        }
    }
    let mut result = source.to_string();
    for edit in &accepted {
        let before = &result[..edit.span.start];
        let after = &result[edit.span.end..];
        result = format!("{before}{}{after}", edit.replacement);
    }
    result
}

pub(super) fn lint_with_require_header(
    source: &str,
    path: Option<&std::path::Path>,
) -> Vec<LintDiagnostic> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let options = LintOptions {
        file_path: path,
        require_file_header: true,
        complexity_threshold: None,
    };
    lint_with_options(&program, &[], Some(source), &HashSet::new(), &options)
}

mod assert_pipeline;
mod autofix;
mod boolean_patterns;
mod break_loop;
mod complexity;
mod empty_blocks;
mod file_header;
mod formatting;
mod harndoc;
mod imports;
mod invalid_binop;
mod llm_rules;
mod mutability;
mod naming_types;
mod redundant_nil_ternary;
mod shadowing;
mod unreachable;
mod unused;
mod unused_function;

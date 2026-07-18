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

/// Run a source fixture through Harn's strict parse/typecheck/compile boundary
/// and then the real VM. Autofix tests use this to prove they preserve runtime
/// behavior instead of merely producing replacement text.
pub(super) fn execute_strict_source(source: &str) -> String {
    let chunk = harn_vm::compile_source(source).expect("fixture should strictly compile");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                vm.execute(&chunk).await.expect("fixture should execute");
                vm.output().trim_end().to_string()
            })
            .await
    })
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
        ..Default::default()
    };
    lint_with_options(&program, &[], Some(source), &HashSet::new(), &options)
}

pub(super) fn lint_with_docstrings(source: &str) -> Vec<LintDiagnostic> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let options = LintOptions {
        require_docstrings: true,
        ..Default::default()
    };
    lint_with_options(&program, &[], Some(source), &HashSet::new(), &options)
}

pub(super) fn lint_with_stdlib_metadata(source: &str) -> Vec<LintDiagnostic> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let options = LintOptions {
        require_stdlib_metadata: true,
        ..Default::default()
    };
    lint_with_options(&program, &[], Some(source), &HashSet::new(), &options)
}

pub(super) fn lint_with_stdlib_return_types(source: &str) -> Vec<LintDiagnostic> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let options = LintOptions {
        require_stdlib_metadata: true,
        ..Default::default()
    };
    lint_with_options(&program, &[], Some(source), &HashSet::new(), &options)
}

mod ambient_capabilities;
mod ambient_clock;
mod ambient_stdio;
mod assert_pipeline;
mod autofix;
mod boolean_patterns;
mod break_loop;
mod complexity;
mod connector_effect_policy;
mod empty_blocks;
mod engine_rules;
mod file_header;
mod formatting;
mod harndoc;
mod hitl;
mod hygiene;
mod imports;
mod invalid_binop;
mod llm_rules;
mod long_running;
mod mcp_tools;
mod mutability;
mod naming_types;
mod nil_coalesce;
mod optional_shorthand;
mod persona_steps;
mod redundant_nil_ternary;
mod secret_scan_rules;
mod shadowing;
mod stdlib_metadata;
mod stdlib_return_types;
mod unnecessary_cast;
mod unnecessary_parentheses;
mod unreachable;
mod unused;
mod unused_function;

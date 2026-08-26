//! Type-checker test suite, split by topic.
//!
//! Shared helpers live here; each submodule re-exports them via
//! `use super::*;` and exercises one slice of the type checker.

use std::collections::HashSet;

use crate::diagnostic_codes::Code;
use crate::Parser;
use harn_lexer::Lexer;

use super::{DiagnosticSeverity, TypeChecker, TypeDiagnostic};

mod acp_ambient_globals;
mod attributes;
mod callable_attributes;
mod calls_and_generics;
mod coalesce;
mod enum_construct;
mod exhaustiveness;
mod flow_predicates;
mod harness_capabilities;
mod implicit_any_parameters;
mod imports;
mod interfaces;
mod lexical_capture;
mod literal_union_args;
mod main_signature;
mod narrowing;
mod nested_generic_alias_inference;
mod nil_safety;
mod ownership;
mod pipeline_typing;
mod reachability;
mod record_arguments;
mod repair;
mod row_merge;
mod soundness;
mod strict_types;
mod throws;
mod tuples;
mod typing;
mod value_calls;

pub(super) fn check_source_raw(source: &str) -> Vec<TypeDiagnostic> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    TypeChecker::new().check(&program)
}

pub(super) fn check_source(source: &str) -> Vec<TypeDiagnostic> {
    without_implicit_parameter_errors(check_source_raw(source))
}

/// Keep pre-existing typechecker tests scoped to the semantic contract they
/// name. The dedicated `implicit_any_parameters` module uses the raw checker
/// output and owns exact coverage for HARN-TYP-028.
fn without_implicit_parameter_errors(diagnostics: Vec<TypeDiagnostic>) -> Vec<TypeDiagnostic> {
    diagnostics
        .into_iter()
        .filter(|diagnostic| diagnostic.code != Code::ImplicitAnyParameter)
        .collect()
}

pub(super) fn diagnostics_with_code(
    source: &str,
    code: Code,
    severity: DiagnosticSeverity,
) -> Vec<TypeDiagnostic> {
    check_source(source)
        .into_iter()
        .filter(|diagnostic| diagnostic.code == code && diagnostic.severity == severity)
        .collect()
}

pub(super) fn check_source_with_imports(source: &str, imported: &[&str]) -> Vec<TypeDiagnostic> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let imports: HashSet<String> = imported.iter().map(|s| s.to_string()).collect();
    without_implicit_parameter_errors(
        TypeChecker::new()
            .with_imported_names(imports)
            .check(&program),
    )
}

pub(super) fn errors(source: &str) -> Vec<String> {
    check_source(source)
        .into_iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| d.message)
        .collect()
}

pub(super) fn warnings(source: &str) -> Vec<String> {
    check_source(source)
        .into_iter()
        .filter(|d| d.severity == DiagnosticSeverity::Warning)
        .map(|d| d.message)
        .collect()
}

pub(super) fn exhaustive_warns(source: &str) -> Vec<String> {
    warnings(source)
        .into_iter()
        .filter(|w| w.contains("was not fully narrowed"))
        .collect()
}

pub(super) fn iface_errors(source: &str) -> Vec<String> {
    errors(source)
        .into_iter()
        .filter(|m| m.contains("does not satisfy interface"))
        .collect()
}

pub(super) fn check_source_with_source(source: &str) -> Vec<TypeDiagnostic> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    without_implicit_parameter_errors(TypeChecker::new().check_with_source(&program, source))
}

pub(super) fn check_source_strict(source: &str) -> Vec<TypeDiagnostic> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    without_implicit_parameter_errors(TypeChecker::with_strict_types(true).check(&program))
}

pub(super) fn strict_errors(source: &str) -> Vec<String> {
    check_source_strict(source)
        .into_iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| d.message)
        .collect()
}

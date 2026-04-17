//! Type-checker test suite, split by topic.
//!
//! Shared helpers live here; each submodule re-exports them via
//! `use super::*;` and exercises one slice of the type checker.

use std::collections::HashSet;

use crate::Parser;
use harn_lexer::Lexer;

use super::{DiagnosticSeverity, TypeChecker, TypeDiagnostic};

mod exhaustiveness;
mod interfaces;
mod narrowing;
mod reachability;
mod strict_types;
mod typing;

pub(super) fn check_source(source: &str) -> Vec<TypeDiagnostic> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    TypeChecker::new().check(&program)
}

pub(super) fn check_source_with_imports(source: &str, imported: &[&str]) -> Vec<TypeDiagnostic> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let imports: HashSet<String> = imported.iter().map(|s| s.to_string()).collect();
    TypeChecker::new()
        .with_imported_names(imports)
        .check(&program)
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
    TypeChecker::new().check_with_source(&program, source)
}

pub(super) fn check_source_strict(source: &str) -> Vec<TypeDiagnostic> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    TypeChecker::with_strict_types(true).check(&program)
}

pub(super) fn strict_warnings(source: &str) -> Vec<String> {
    check_source_strict(source)
        .into_iter()
        .filter(|d| d.severity == DiagnosticSeverity::Warning)
        .map(|d| d.message)
        .collect()
}

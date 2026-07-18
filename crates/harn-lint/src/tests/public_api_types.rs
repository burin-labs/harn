use std::collections::{HashMap, HashSet};

use harn_lexer::Lexer;
use harn_parser::{DiagnosticCode as Code, Parser};

use super::*;

fn lint_public_api(source: &str, require_stdlib_metadata: bool) -> Vec<LintDiagnostic> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let options = LintOptions {
        require_public_api_types: true,
        require_stdlib_metadata,
        ..Default::default()
    };
    lint_with_options(&program, &[], Some(source), &HashSet::new(), &options)
}

fn public_api_diagnostics(source: &str) -> Vec<LintDiagnostic> {
    lint_public_api(source, false)
        .into_iter()
        .filter(|diagnostic| diagnostic.code == Code::LintMissingPublicApiType)
        .collect()
}

#[test]
fn public_api_type_policy_is_opt_in() {
    let diagnostics = lint_source("pub fn run(value) {}\npub pipeline deploy(task) {}\n");
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != Code::LintMissingPublicApiType),
        "default lint must preserve legacy declarations: {diagnostics:?}"
    );
}

#[test]
fn fully_typed_public_apis_and_private_inference_are_clean() {
    let diagnostics = public_api_diagnostics(
        r"
pub fn run(value: unknown) -> any { return value }
pub pipeline deploy(config: dict) -> bool { return true }
fn private_helper(value) { return value }
pipeline private_pipeline(task) {}
tool internal_tool(value) { return value }
",
    );
    assert!(
        diagnostics.is_empty(),
        "unexpected diagnostics: {diagnostics:?}"
    );
}

#[test]
fn reports_every_missing_public_parameter_and_return_without_023_duplication() {
    let source =
        "pub fn run(first, second: int) {}\npub pipeline test_publish(task, count: int) {}\n";
    let all = lint_public_api(source, false);
    let diagnostics: Vec<_> = all
        .iter()
        .filter(|diagnostic| diagnostic.code == Code::LintMissingPublicApiType)
        .collect();
    assert_eq!(diagnostics.len(), 4, "diagnostics: {all:?}");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.message.contains("parameter `first`")
            && &source[diagnostic.span.start..diagnostic.span.end] == "first"
    }));
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.message.contains("fn `run` is missing")
            && &source[diagnostic.span.start..diagnostic.span.end] == "run"
    }));
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.message.contains("parameter `task`")
            && &source[diagnostic.span.start..diagnostic.span.end] == "task"
    }));
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("pipeline `test_publish` is missing")
            && &source[diagnostic.span.start..diagnostic.span.end] == "test_publish"
    }));
    assert!(
        all.iter()
            .all(|diagnostic| diagnostic.code != Code::LintPipelineReturnType),
        "HARN-LNT-067 must own the pipeline return under this mode: {all:?}"
    );
}

#[test]
fn conventional_entry_and_test_pipelines_are_not_exempt() {
    let diagnostics = public_api_diagnostics(
        "pub pipeline default(task) {}\npub pipeline test_publish(task) {}\n",
    );
    assert_eq!(diagnostics.len(), 4, "diagnostics: {diagnostics:?}");
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message.contains("pipeline `default` parameter")));
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message.contains("pipeline `default` is missing")));
}

#[test]
fn parameter_spans_remain_byte_exact_with_nested_types_and_defaults() {
    let source = "// π\npub fn collect(first: list<{left: int, right: list<string>}>, fallback, final = {value: 1}) -> nil { return nil }\n";
    let diagnostics = public_api_diagnostics(source);
    let slices: Vec<_> = diagnostics
        .iter()
        .map(|diagnostic| &source[diagnostic.span.start..diagnostic.span.end])
        .collect();
    assert_eq!(
        slices,
        ["fallback", "final"],
        "diagnostics: {diagnostics:?}"
    );
}

#[test]
fn stdlib_return_diagnostic_remains_the_existing_owner() {
    let diagnostics = lint_public_api("pub fn run(value) {}\n", true);
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == Code::LintMissingStdlibReturnType));
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == Code::LintMissingPublicApiType)
            .count(),
        1,
        "only the missing parameter should use HARN-LNT-067: {diagnostics:?}"
    );
}

#[test]
fn severity_overrides_apply_to_public_api_type_diagnostics() {
    let source = "pub fn run(value) {}\n";
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let overrides = HashMap::from([("missing-public-api-type".to_string(), LintSeverity::Error)]);
    let options = LintOptions {
        require_public_api_types: true,
        severity_overrides: overrides,
        ..Default::default()
    };

    let diagnostics = lint_with_options(&program, &[], Some(source), &HashSet::new(), &options);
    let owned = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == Code::LintMissingPublicApiType)
        .collect::<Vec<_>>();
    assert_eq!(owned.len(), 2);
    assert!(owned
        .iter()
        .all(|diagnostic| diagnostic.severity == LintSeverity::Error));
}

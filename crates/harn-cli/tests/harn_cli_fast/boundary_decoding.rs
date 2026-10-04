//! Decoding untyped data at the boundary keeps its type through every
//! spelling, and the captured process and HTTP results are typed records.
//!
//! These builtins are registered by `harn-vm`, so the checks run with the
//! runtime manifest installed, as `harn check` does.

use harn_parser::{check_source, DiagnosticDetails, PipelineError, TypeDiagnostic};

fn diagnostics(source: &str) -> Vec<TypeDiagnostic> {
    harn_parser::install_builtin_manifest(harn_vm::stdlib::all_builtin_manifest());
    match check_source(source) {
        Ok((_program, diagnostics)) => diagnostics,
        Err(PipelineError::TypeCheck(diagnostic)) => vec![*diagnostic],
        Err(other) => panic!("unexpected non-type-check pipeline error: {other:?}"),
    }
}

fn rule_count(diagnostics: &[TypeDiagnostic], rule_name: &str) -> usize {
    diagnostics
        .iter()
        .filter(|diagnostic| {
            matches!(
                &diagnostic.details,
                Some(DiagnosticDetails::LintRule { rule }) if *rule == rule_name
            )
        })
        .count()
}

/// Each binding below is a type error only if the decoded type reached it, so
/// a spelling that erases `T` lets a misspelled field read `nil` again.
#[test]
fn decoded_values_keep_their_type_through_each_spelling() {
    let found = diagnostics(
        r#"
type Run = {headBranch: string, url: string}

fn decode(raw: unknown) -> Result<int, any> {
  const via_try = schema_parse(raw, schema_of(Run))?
  const a: int = via_try.url
  const b: int = unwrap(schema_parse(raw, schema_of(Run))).url
  const c: int = unwrap(json_decode("{}", schema_of(Run))).headBranch
  const d: int = unwrap_err(schema_check(raw, schema_of(Run))).message
  const e: int = unwrap_or(schema_parse(raw, schema_of(Run)), nil)
  return Ok(a + b + c + d + e)
}
"#,
    );
    let mismatches: Vec<_> = found
        .iter()
        .filter(|diagnostic| diagnostic.message.contains("expected int"))
        .collect();
    assert_eq!(
        mismatches.len(),
        5,
        "every decode spelling must stay typed, got: {found:#?}"
    );
}

/// A schema built at runtime names no `T`, so its result stays dynamic.
#[test]
fn a_runtime_schema_dict_decodes_to_a_dynamic_value() {
    let found = diagnostics(
        r#"
fn decode(raw: unknown) -> string {
  const parsed = schema_parse(raw, {type: "dict"})
  return unwrap(parsed).anything
}
"#,
    );
    let errors: Vec<_> = found
        .iter()
        .filter(|diagnostic| diagnostic.severity == harn_parser::DiagnosticSeverity::Error)
        .collect();
    assert!(errors.is_empty(), "got: {errors:#?}");
}

/// The captured process and HTTP results have no nil fields, so hedging them
/// is reported by the existing safe-navigation lint.
#[test]
fn hedging_a_typed_process_or_http_result_is_unnecessary() {
    let found = diagnostics(
        r#"
fn main(harness: Harness) {
  const child = harness.process.exec("git", "status")
  const response = harness.net.get("https://example.com")
  harness.stdio.println(child?.stdout)
  harness.stdio.println(response?.body)
}
"#,
    );
    assert_eq!(
        rule_count(&found, "unnecessary-safe-navigation"),
        2,
        "got: {found:#?}"
    );
}

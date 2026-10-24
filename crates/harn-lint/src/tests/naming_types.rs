//! `naming-convention` plus `unused-type` struct checks.

use super::*;

#[test]
fn test_naming_convention_span_anchors_to_function_name() {
    // The name lint should underline `fn BadName`, not the whole multi-line
    // function declaration (same HARN-LNT-002-class span bug).
    let source = "
fn BadName(a: int, b: int, c: string) -> int {
  return a
}
";
    let diags = lint_source(source);
    let warning = diags
        .iter()
        .find(|d| d.rule == "naming-convention")
        .expect("expected naming-convention warning");
    #[expect(clippy::string_slice, reason = "test input is ASCII")]
    let underlined = &source[warning.span.start..warning.span.end];
    assert_eq!(
        underlined, "fn BadName",
        "function name lint must underline only keyword + name, got: {underlined:?}"
    );
}

#[test]
fn test_naming_convention_span_anchors_to_type_name() {
    let source = "
struct bad_name {
  value: int
  other: string
}
";
    let diags = lint_source(source);
    let warning = diags
        .iter()
        .find(|d| d.rule == "naming-convention")
        .expect("expected naming-convention warning");
    #[expect(clippy::string_slice, reason = "test input is ASCII")]
    let underlined = &source[warning.span.start..warning.span.end];
    assert_eq!(
        underlined, "struct bad_name",
        "type name lint must underline only keyword + name, got: {underlined:?}"
    );
}

#[rstest::rstest]
#[case::test_unused_type_warns_for_unreferenced_struct(
    r#"
struct Helper {
  value: int
}

pipeline default(task) {
  log("ready")
}
"#,
    "unused-type",
    true
)]
#[case::test_unused_type_ignores_referenced_struct(
    r"
struct Helper {
  value: int
}

fn build() -> Helper {
  return Helper { value: 1 }
}

pipeline default(task) {
  const item = build()
  log(item.value)
}
",
    "unused-type",
    false
)]
#[case::test_unused_type_warns_for_unreferenced_alias(
    r#"
type Payload = {value: int}

pipeline default(task) {
  log("ready")
}
"#,
    "unused-type",
    true
)]
#[case::test_unused_type_ignores_referenced_alias(
    r"
type Payload = {value: int}

fn build() -> Payload {
  return {value: 1}
}

pipeline default(task) {
  const item = build()
  log(item.value)
}
",
    "unused-type",
    false
)]
#[case::test_unused_type_ignores_binding_annotation_reference(
    r"
type Payload = {value: int}

pipeline default(task) {
  const item: Payload = {value: 1}
  log(item.value)
}
",
    "unused-type",
    false
)]
#[case::test_unused_type_ignores_pipeline_return_annotation_reference(
    r"
type Payload = {value: int}

pipeline default(task) -> Payload {
  return {value: 1}
}
",
    "unused-type",
    false
)]
#[case::test_unused_type_ignores_closure_param_annotation_reference(
    r"
type Payload = {value: int}

pipeline default(task) {
  const read_value = { item: Payload -> item.value }
  log(read_value({value: 1}))
}
",
    "unused-type",
    false
)]
#[case::test_unused_type_ignores_function_call_type_arg_reference(
    r"
type Payload = {value: int}

fn identity<T>(item: T) -> T {
  return item
}

pipeline default(task) {
  const item = identity<Payload>({value: 1})
  log(item.value)
}
",
    "unused-type",
    false
)]
#[case::test_unused_type_ignores_schema_of_reference(
    r"
type Payload = {value: int}

pipeline default(task) {
  const schema = schema_of(Payload)
  log(schema)
}
",
    "unused-type",
    false
)]
#[case::test_unused_type_ignores_schema_predicate_reference(
    r"
type Payload = {value: int}

pipeline default(task) {
  const value: unknown = {value: 1}
  if schema_is(value, Payload) {
    log(value.value)
  }
}
",
    "unused-type",
    false
)]
#[case::test_unused_type_ignores_typed_catch_reference(
    r#"
type AppError = {message: string}

pipeline default(task) {
  try {
    throw {message: "boom"}
  } catch (err: AppError) {
    log(err.message)
  }
}
"#,
    "unused-type",
    false
)]
fn rule_presence(#[case] source: &str, #[case] rule: &str, #[case] present: bool) {
    let diagnostics = lint_source(source);
    assert_eq!(
        has_rule(&diagnostics, rule),
        present,
        "{rule}: {diagnostics:?}"
    );
}

fn lint_at_path(source: &str, path: &str) -> Vec<LintDiagnostic> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let path = std::path::PathBuf::from(path);
    let options = LintOptions {
        file_path: Some(&path),
        ..Default::default()
    };
    lint_with_options(&program, &[], Some(source), &HashSet::new(), &options)
}

#[test]
fn test_generated_file_skips_style_lints() {
    let source = "type Payload = {value: int}\n";
    // A hand-authored file still flags the unused type.
    assert!(
        has_rule(&lint_at_path(source, "schema.harn"), "unused-type"),
        "plain .harn should flag unused-type"
    );
    // The same content in a `*.generated.harn` file skips style lints.
    assert!(
        !has_rule(
            &lint_at_path(source, "schema.generated.harn"),
            "unused-type"
        ),
        "*.generated.harn should skip the unused-type style lint"
    );
}

#[test]
fn test_is_generated_path_matches_only_suffix() {
    use std::path::Path;
    assert!(is_generated_path(Path::new("db_types.generated.harn")));
    assert!(is_generated_path(Path::new("/a/b/schema.generated.harn")));
    // A plain .harn, or a name that merely contains "generated", is not exempt.
    assert!(!is_generated_path(Path::new("db_types.harn")));
    assert!(!is_generated_path(Path::new("generated.harn")));
    assert!(!is_generated_path(Path::new("my_generated_types.harn")));
}

#[test]
fn test_path_is_stdlib_source_matches_canonical_embedded_tree() {
    use std::path::Path;
    assert!(path_is_stdlib_source(Path::new(
        "crates/harn-stdlib/src/stdlib/stdlib_fs.harn"
    )));
    assert!(path_is_stdlib_source(Path::new(
        "/abs/path/crates/harn-stdlib/src/stdlib/agent/loop.harn"
    )));
    assert!(!path_is_stdlib_source(Path::new("scripts/foo.harn")));
    assert!(!path_is_stdlib_source(Path::new(
        "crates/harn-vm/src/stdlib_acp.harn"
    )));
    assert!(!path_is_stdlib_source(Path::new(
        "conformance/tests/foo.harn"
    )));
}

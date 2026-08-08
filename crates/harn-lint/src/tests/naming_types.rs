//! `naming-convention` plus `unused-type` struct checks.

use super::*;

#[test]
fn test_naming_convention_flags_non_snake_case_function() {
    let diags = lint_source(
        r"
fn BadName() {
  return nil
}
",
    );
    assert!(
        has_rule(&diags, "naming-convention"),
        "expected naming-convention warning, got: {diags:?}"
    );
}

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
    let underlined = &source[warning.span.start..warning.span.end];
    assert_eq!(
        underlined, "struct bad_name",
        "type name lint must underline only keyword + name, got: {underlined:?}"
    );
}

#[test]
fn test_naming_convention_flags_non_pascal_case_type() {
    let diags = lint_source(
        r"
struct bad_name {
  value: int
}
",
    );
    assert!(
        has_rule(&diags, "naming-convention"),
        "expected naming-convention warning, got: {diags:?}"
    );
}

#[test]
fn test_unused_type_warns_for_unreferenced_struct() {
    let diags = lint_source(
        r#"
struct Helper {
  value: int
}

pipeline default(task) {
  log("ready")
}
"#,
    );
    assert!(
        has_rule(&diags, "unused-type"),
        "expected unused-type warning, got: {diags:?}"
    );
}

#[test]
fn test_unused_type_ignores_referenced_struct() {
    let diags = lint_source(
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
    );
    assert!(
        !has_rule(&diags, "unused-type"),
        "referenced types should not trigger unused-type: {diags:?}"
    );
}

#[test]
fn test_unused_type_warns_for_unreferenced_alias() {
    let diags = lint_source(
        r#"
type Payload = {value: int}

pipeline default(task) {
  log("ready")
}
"#,
    );
    assert!(
        has_rule(&diags, "unused-type"),
        "expected unused-type warning for alias, got: {diags:?}"
    );
}

#[test]
fn test_unused_type_ignores_referenced_alias() {
    let diags = lint_source(
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
    );
    assert!(
        !has_rule(&diags, "unused-type"),
        "referenced aliases should not trigger unused-type: {diags:?}"
    );
}

#[test]
fn test_unused_type_ignores_binding_annotation_reference() {
    let diags = lint_source(
        r"
type Payload = {value: int}

pipeline default(task) {
  const item: Payload = {value: 1}
  log(item.value)
}
",
    );
    assert!(
        !has_rule(&diags, "unused-type"),
        "types referenced by binding annotations should not trigger unused-type: {diags:?}"
    );
}

#[test]
fn test_unused_type_ignores_pipeline_return_annotation_reference() {
    let diags = lint_source(
        r"
type Payload = {value: int}

pipeline default(task) -> Payload {
  return {value: 1}
}
",
    );
    assert!(
        !has_rule(&diags, "unused-type"),
        "types referenced by pipeline return annotations should not trigger unused-type: {diags:?}"
    );
}

#[test]
fn test_unused_type_ignores_closure_param_annotation_reference() {
    let diags = lint_source(
        r"
type Payload = {value: int}

pipeline default(task) {
  const read_value = { item: Payload -> item.value }
  log(read_value({value: 1}))
}
",
    );
    assert!(
        !has_rule(&diags, "unused-type"),
        "types referenced by closure parameter annotations should not trigger unused-type: {diags:?}"
    );
}

#[test]
fn test_unused_type_ignores_function_call_type_arg_reference() {
    let diags = lint_source(
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
    );
    assert!(
        !has_rule(&diags, "unused-type"),
        "types referenced by call type arguments should not trigger unused-type: {diags:?}"
    );
}

#[test]
fn test_unused_type_ignores_schema_of_reference() {
    let diags = lint_source(
        r"
type Payload = {value: int}

pipeline default(task) {
  const schema = schema_of(Payload)
  log(schema)
}
",
    );
    assert!(
        !has_rule(&diags, "unused-type"),
        "types referenced by schema_of(T) should not trigger unused-type: {diags:?}"
    );
}

#[test]
fn test_unused_type_ignores_typed_catch_reference() {
    let diags = lint_source(
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
    );
    assert!(
        !has_rule(&diags, "unused-type"),
        "types referenced by typed catch clauses should not trigger unused-type: {diags:?}"
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

//! `cyclomatic-complexity` — default threshold, overrides, and
//! `@complexity(allow)` suppression.

use super::*;

#[test]
fn test_cyclomatic_complexity_warns_for_branchy_function() {
    // 26 ifs → score 27, above the default threshold of 25.
    let diags = lint_source(
        r#"
fn complicated(x: int) {
  if x > 0 { log("1") }
  if x > 1 { log("2") }
  if x > 2 { log("3") }
  if x > 3 { log("4") }
  if x > 4 { log("5") }
  if x > 5 { log("6") }
  if x > 6 { log("7") }
  if x > 7 { log("8") }
  if x > 8 { log("9") }
  if x > 9 { log("10") }
  if x > 10 { log("11") }
  if x > 11 { log("12") }
  if x > 12 { log("13") }
  if x > 13 { log("14") }
  if x > 14 { log("15") }
  if x > 15 { log("16") }
  if x > 16 { log("17") }
  if x > 17 { log("18") }
  if x > 18 { log("19") }
  if x > 19 { log("20") }
  if x > 20 { log("21") }
  if x > 21 { log("22") }
  if x > 22 { log("23") }
  if x > 23 { log("24") }
  if x > 24 { log("25") }
  if x > 25 { log("26") }
}

pipeline default(task) {
  complicated(10)
}
"#,
    );
    assert!(
        has_rule(&diags, "cyclomatic-complexity"),
        "expected cyclomatic-complexity warning, got: {diags:?}"
    );
}

#[test]
fn test_cyclomatic_complexity_quiet_below_default_threshold() {
    // 11 ifs → score 12. Under the 10-era default this would warn;
    // with the bumped default of 25 it should stay quiet.
    let diags = lint_source(
        r#"
fn branchy(x: int) {
  if x > 0 { log("1") }
  if x > 1 { log("2") }
  if x > 2 { log("3") }
  if x > 3 { log("4") }
  if x > 4 { log("5") }
  if x > 5 { log("6") }
  if x > 6 { log("7") }
  if x > 7 { log("8") }
  if x > 8 { log("9") }
  if x > 9 { log("10") }
  if x > 10 { log("11") }
}

pipeline default(task) {
  branchy(10)
}
"#,
    );
    assert!(
        !has_rule(&diags, "cyclomatic-complexity"),
        "score 12 must not trigger the default-25 threshold: {diags:?}"
    );
}

#[test]
fn test_cyclomatic_complexity_threshold_override_stricter() {
    // 11 ifs → score 12. With a project threshold of 5 this should warn.
    let source = r#"
fn branchy(x: int) {
  if x > 0 { log("1") }
  if x > 1 { log("2") }
  if x > 2 { log("3") }
  if x > 3 { log("4") }
  if x > 4 { log("5") }
  if x > 5 { log("6") }
  if x > 6 { log("7") }
  if x > 7 { log("8") }
  if x > 8 { log("9") }
  if x > 9 { log("10") }
  if x > 10 { log("11") }
}

pipeline default(task) {
  branchy(10)
}
"#;
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let externally_imported: HashSet<String> = HashSet::new();
    let options = LintOptions {
        file_path: None,
        require_file_header: false,
        complexity_threshold: Some(5),
    };
    let diags = lint_with_options(&program, &[], Some(source), &externally_imported, &options);
    let complexity_warnings: Vec<_> = diags
        .iter()
        .filter(|d| d.rule == "cyclomatic-complexity")
        .collect();
    assert_eq!(
        complexity_warnings.len(),
        1,
        "expected exactly one complexity warning with threshold=5, got: {diags:?}"
    );
    let msg = &complexity_warnings[0].message;
    assert!(
        msg.contains("(> 5)"),
        "threshold should be reflected in the diagnostic, got: {msg}"
    );
}

#[test]
fn test_cyclomatic_complexity_threshold_override_permissive() {
    // Same branchy function as the default test (score 27). With a
    // project threshold of 100, it should stay quiet.
    let source = r#"
fn complicated(x: int) {
  if x > 0 { log("1") }
  if x > 1 { log("2") }
  if x > 2 { log("3") }
  if x > 3 { log("4") }
  if x > 4 { log("5") }
  if x > 5 { log("6") }
  if x > 6 { log("7") }
  if x > 7 { log("8") }
  if x > 8 { log("9") }
  if x > 9 { log("10") }
  if x > 10 { log("11") }
  if x > 11 { log("12") }
  if x > 12 { log("13") }
  if x > 13 { log("14") }
  if x > 14 { log("15") }
  if x > 15 { log("16") }
  if x > 16 { log("17") }
  if x > 17 { log("18") }
  if x > 18 { log("19") }
  if x > 19 { log("20") }
  if x > 20 { log("21") }
  if x > 21 { log("22") }
  if x > 22 { log("23") }
  if x > 23 { log("24") }
  if x > 24 { log("25") }
  if x > 25 { log("26") }
}

pipeline default(task) {
  complicated(10)
}
"#;
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let externally_imported: HashSet<String> = HashSet::new();
    let options = LintOptions {
        file_path: None,
        require_file_header: false,
        complexity_threshold: Some(100),
    };
    let diags = lint_with_options(&program, &[], Some(source), &externally_imported, &options);
    assert!(
        !has_rule(&diags, "cyclomatic-complexity"),
        "threshold=100 must silence the warning: {diags:?}"
    );
}

#[test]
fn test_cyclomatic_complexity_allow_attribute_suppresses() {
    // Intentionally branchy parser-style function; `@complexity(allow)`
    // must silence the warning even when the score is above threshold.
    let diags = lint_source(
        r#"
@complexity(allow)
fn dispatch(x: int) {
  if x == 0 { log("0") }
  if x == 1 { log("1") }
  if x == 2 { log("2") }
  if x == 3 { log("3") }
  if x == 4 { log("4") }
  if x == 5 { log("5") }
  if x == 6 { log("6") }
  if x == 7 { log("7") }
  if x == 8 { log("8") }
  if x == 9 { log("9") }
  if x == 10 { log("10") }
  if x == 11 { log("11") }
  if x == 12 { log("12") }
  if x == 13 { log("13") }
  if x == 14 { log("14") }
  if x == 15 { log("15") }
  if x == 16 { log("16") }
  if x == 17 { log("17") }
  if x == 18 { log("18") }
  if x == 19 { log("19") }
  if x == 20 { log("20") }
  if x == 21 { log("21") }
  if x == 22 { log("22") }
  if x == 23 { log("23") }
  if x == 24 { log("24") }
  if x == 25 { log("25") }
  if x == 26 { log("26") }
}

pipeline default(task) {
  dispatch(1)
}
"#,
    );
    assert!(
        !has_rule(&diags, "cyclomatic-complexity"),
        "@complexity(allow) must suppress the warning: {diags:?}"
    );
}

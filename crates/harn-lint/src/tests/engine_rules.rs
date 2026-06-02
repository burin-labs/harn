//! Engine rules run as lint rules (#2849): a declarative `harn-rules` rule
//! supplied via `LintOptions::engine_rules` shows up in lint output like a
//! built-in, with its fix and `disable_rules` filtering.

use super::*;

const NO_FOO: &str = "id = \"no-foo\"\nlanguage = \"harn\"\nseverity = \"warning\"\nmessage = \"no foo calls\"\nfix = \"bar()\"\n[rule]\npattern = \"foo()\"\n";

fn lint_with_engine_rules(
    source: &str,
    rules: &[String],
    disabled: &[String],
) -> Vec<LintDiagnostic> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let options = LintOptions {
        engine_rules: rules,
        ..Default::default()
    };
    lint_with_options(&program, disabled, Some(source), &HashSet::new(), &options)
}

#[test]
fn severity_override_remaps_a_rule() {
    // `no-foo` defaults to `warning`; a project override promotes it to `error`
    // (#2851). Applied after disable-filtering, by rule id.
    use std::collections::HashMap;
    let rules = vec![NO_FOO.to_string()];
    let mut overrides: HashMap<String, crate::diagnostic::LintSeverity> = HashMap::new();
    overrides.insert("no-foo".to_string(), crate::diagnostic::LintSeverity::Error);

    let source = "fn main() {\n  foo()\n}\n";
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let options = LintOptions {
        engine_rules: &rules,
        severity_overrides: overrides,
        ..Default::default()
    };
    let diags = lint_with_options(&program, &[], Some(source), &HashSet::new(), &options);
    let d = diags
        .iter()
        .find(|d| d.rule == "no-foo")
        .expect("no-foo fired");
    assert_eq!(d.severity, crate::diagnostic::LintSeverity::Error);
}

#[test]
fn engine_rule_emits_a_lint_with_its_fix() {
    let rules = vec![NO_FOO.to_string()];
    let diags = lint_with_engine_rules("fn main() {\n  foo()\n}\n", &rules, &[]);
    assert!(has_rule(&diags, "no-foo"), "diags: {diags:?}");
    let d = diags.iter().find(|d| d.rule == "no-foo").unwrap();
    assert_eq!(d.message, "no foo calls");
    assert_eq!(d.severity, crate::diagnostic::LintSeverity::Warning);
    assert_eq!(d.code, harn_parser::DiagnosticCode::LintRuleEngine);
    let fix = d
        .fix
        .as_ref()
        .expect("engine rule with a `fix` is a lint fix");
    assert_eq!(fix.len(), 1);
    assert_eq!(fix[0].replacement, "bar()");
}

#[test]
fn engine_rule_is_filtered_by_disable_rules() {
    let rules = vec![NO_FOO.to_string()];
    let diags =
        lint_with_engine_rules("fn main() {\n  foo()\n}\n", &rules, &["no-foo".to_string()]);
    assert!(!has_rule(&diags, "no-foo"), "disable_rules should hide it");
}

#[test]
fn malformed_engine_rule_is_skipped_not_fatal() {
    let rules = vec!["this is not [[[ valid rule toml".to_string()];
    // Linting still succeeds; the bad rule simply contributes nothing.
    let diags = lint_with_engine_rules("fn main() {\n  foo()\n}\n", &rules, &[]);
    assert!(!has_rule(&diags, "no-foo"));
}

#[test]
fn engine_rule_fix_applies_cleanly() {
    let rules = vec![NO_FOO.to_string()];
    let source = "fn main() {\n  foo()\n}\n";
    let diags = lint_with_engine_rules(source, &rules, &[]);
    let fixed = apply_fixes(source, &diags);
    assert_eq!(fixed, "fn main() {\n  bar()\n}\n");
}

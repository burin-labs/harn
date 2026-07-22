//! Acceptance for #2835: safety classification, the
//! machine-applicable/suggestion gate, and the idempotency check.

use harn_rules::{Applicability, CompiledRule, Rule, RulesError, Safety};

fn compile(toml: &str) -> CompiledRule {
    CompiledRule::compile(&Rule::from_toml_str(toml).expect("parses")).expect("compiles")
}

fn codemod(safety: &str) -> CompiledRule {
    compile(&format!(
        r#"
        id = "rename"
        language = "typescript"
        safety = "{safety}"
        fix = "bar()"
        [rule]
        pattern = "foo()"
        "#
    ))
}

#[test]
fn default_safety_refuses_auto_apply() {
    // No `safety` declared -> defaults to scope-local -> a suggestion.
    let rule = compile(
        r#"
        id = "rename"
        language = "typescript"
        fix = "bar()"
        [rule]
        pattern = "foo()"
        "#,
    );
    assert_eq!(rule.safety(), Safety::ScopeLocal);
    assert_eq!(rule.applicability(), Applicability::Suggestion);
    // The preview is still available …
    let preview = rule.apply("foo();").unwrap();
    assert!(preview.rewritten.contains("bar()"));
    // … but auto_apply refuses without an explicit opt-in.
    assert!(matches!(
        rule.auto_apply("foo();"),
        Err(RulesError::NotAutoApplicable { .. })
    ));
}

#[test]
fn machine_applicable_tiers_auto_apply() {
    for safety in ["format-only", "behavior-preserving"] {
        let rule = codemod(safety);
        assert_eq!(rule.applicability(), Applicability::MachineApplicable);
        let applied = rule.auto_apply("foo();").expect("auto-applies");
        assert!(applied.rewritten.contains("bar()"));
        assert_eq!(applied.safety, rule.safety());
    }
}

#[test]
fn risky_tiers_are_suggestions() {
    for safety in [
        "scope-local",
        "surface-changing",
        "capability-changing",
        "needs-human",
    ] {
        let rule = codemod(safety);
        assert_eq!(rule.applicability(), Applicability::Suggestion);
        assert!(rule.auto_apply("foo();").is_err());
    }
}

#[test]
fn idempotent_fix_passes_the_check() {
    let rule = codemod("format-only");
    let result = rule.apply("foo();\n").unwrap();
    assert!(result.idempotent);
    // `bar()` no longer matches `foo()`, so a second pass is a no-op.
    assert!(rule.apply_checked("foo();\n").is_ok());
}

#[test]
fn non_idempotent_fix_is_rejected() {
    // `foo()` -> `foo()()` reintroduces a `foo()` match on every pass, so it
    // never reaches a fixed point.
    let rule = compile(
        r#"
        id = "double-call"
        language = "typescript"
        safety = "behavior-preserving"
        fix = "foo()()"
        [rule]
        pattern = "foo()"
        "#,
    );
    let result = rule.apply("foo();\n").unwrap();
    assert!(!result.idempotent, "fix should be flagged non-idempotent");
    assert!(matches!(
        rule.apply_checked("foo();\n"),
        Err(RulesError::NotIdempotent { .. })
    ));
}

#[test]
fn diagnostics_carry_message_severity_and_applicability() {
    let rule = compile(
        r#"
        id = "no-foo"
        language = "typescript"
        severity = "error"
        message = "Do not call foo()"
        safety = "behavior-preserving"
        fix = "bar()"
        [rule]
        pattern = "foo()"
        "#,
    );
    let diags = rule.diagnostics("foo();\nfoo();\n").unwrap();
    assert_eq!(diags.len(), 2);
    assert_eq!(diags[0].message, "Do not call foo()");
    assert_eq!(diags[0].rule_id, "no-foo");
    assert_eq!(diags[0].applicability, Applicability::MachineApplicable);
    assert_eq!(diags[0].fix.as_deref(), Some("bar()"));
}

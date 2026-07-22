//! The curated `rules/std` seed pack (#2844) is part of the engine's
//! reference corpus — so it ships with tests that fail loudly on a
//! regression. Each rule is loaded from its shipped `*.toml` and exercised.

use std::path::PathBuf;

use harn_rules::{load_rule_file, CompiledRule};

fn seed(name: &str) -> CompiledRule {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../rules/std")
        .join(name);
    let rule = load_rule_file(&path).unwrap_or_else(|e| panic!("load {}: {e}", path.display()));
    CompiledRule::compile(&rule).unwrap_or_else(|e| panic!("compile {}: {e}", path.display()))
}

#[test]
fn destructure_defaults_folds_the_non_alias_case() {
    let rule = seed("destructure-defaults.toml");

    // The flagship #2824 shape: binding name == property name.
    let out = rule
        .apply("const timeout = cfg?.timeout ?? 30;\n")
        .expect("apply");
    assert!(out.changed);
    assert_eq!(out.rewritten, "const { timeout = 30 } = cfg;\n");
}

#[test]
fn destructure_defaults_leaves_aliases_alone() {
    let rule = seed("destructure-defaults.toml");
    // Binding name (`t`) differs from the property (`timeout`) — `$K` cannot
    // unify, so the rule must not touch it.
    let out = rule
        .apply("const t = cfg?.timeout ?? 30;\n")
        .expect("apply");
    assert!(
        !out.changed,
        "alias case must be left alone: {}",
        out.rewritten
    );
}

#[test]
fn destructure_defaults_is_a_suggestion_not_auto_applied() {
    // The rewrite is unsafe when `$X` is nullish, so it must not auto-apply.
    let rule = seed("destructure-defaults.toml");
    assert!(!rule.safety().is_auto_applicable());
}

#[test]
fn no_console_log_flags_calls() {
    let rule = seed("no-console-log.toml");
    let diags = rule
        .diagnostics("console.log(x);\nfoo();\nconsole.log(y, z);\n")
        .expect("diagnostics");
    assert_eq!(diags.len(), 2);
    assert_eq!(diags[0].message, "Remove `console.log` before committing");
    // A lint has no fix.
    assert!(diags[0].fix.is_none());
}

//! End-to-end atomic-tier acceptance for #2832: load a rule from a TOML
//! file, compile its `pattern` snippet into a tree-sitter query, and run it
//! to produce matches with metavariable bindings — the same path
//! `ast.search` (#2830) takes, but driven by a declarative rule.

use std::path::PathBuf;

use harn_rules::{load_rule_file, CompiledRule, Rule, RuleKind};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn destructuring_rule_round_trips_to_matches() {
    // 1) Load the declarative rule (the #2824 / burin-code#1629 customer).
    let rule = load_rule_file(fixture("destructure_default.toml")).expect("rule loads");
    assert_eq!(rule.id, "destructure-with-defaults");
    // `fix` present -> codemod.
    assert_eq!(rule.kind(), RuleKind::Codemod);

    // 2) Compile the `$SRC?.$KEY ?? $DEFAULT` snippet into a query.
    let compiled = CompiledRule::compile(&rule).expect("rule compiles");

    // 3) Run it over a fixture and read every capture by name.
    let source = "const a = cfg?.timeout ?? 30;\nconst b = opts?.retries ?? 3;\n";
    let matches = compiled.run(source).expect("runs");
    assert_eq!(matches.len(), 2);

    assert_eq!(matches[0].bindings["SRC"].text, "cfg");
    assert_eq!(matches[0].bindings["KEY"].text, "timeout");
    assert_eq!(matches[0].bindings["DEFAULT"].text, "30");
    assert_eq!(matches[0].text, "cfg?.timeout ?? 30");

    assert_eq!(matches[1].bindings["SRC"].text, "opts");
    assert_eq!(matches[1].bindings["KEY"].text, "retries");
    assert_eq!(matches[1].bindings["DEFAULT"].text, "3");

    // The bound metavars carry the spans the #2834 `fix` interpolation will
    // splice from — `{ $KEY: $SRC }` reads `KEY`/`SRC` here.
    assert_eq!(matches[0].bindings["KEY"].span.start_row, 0);
    assert_eq!(matches[1].bindings["KEY"].span.start_row, 1);
}

#[test]
fn typed_placeholder_rule_round_trips_through_toml() {
    // The #2839 acceptance, end-to-end through the declarative TOML path: a
    // `$VAR:kind` constraint narrows the matcher to a syntactic class.
    let rule = Rule::from_toml_str(
        r#"
        id = "log-of-identifier"
        language = "typescript"
        [rule]
        pattern = "log($ARG:identifier)"
        "#,
    )
    .expect("rule parses");
    let compiled = CompiledRule::compile(&rule).expect("rule compiles");

    // Binds when the argument is a bare identifier …
    let hit = compiled.run("log(value);\n").expect("runs");
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].bindings["ARG"].text, "value");

    // … but not when it is a call expression — the kind constraint holds.
    let miss = compiled.run("log(compute());\n").expect("runs");
    assert!(miss.is_empty(), "`:identifier` must reject a call argument");
}

#[test]
fn the_pattern_is_operator_precise() {
    // The compiled query constrains the `??` operator, so the structurally
    // identical `||` form is left alone — the codemod stays sound.
    let rule = load_rule_file(fixture("destructure_default.toml")).unwrap();
    let compiled = CompiledRule::compile(&rule).unwrap();
    let matches = compiled.run("const a = cfg?.timeout || 30;\n").unwrap();
    assert!(matches.is_empty(), "`||` must not match the `??` rule");
}

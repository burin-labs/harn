//! Acceptance for #2833: the relational (`inside` / `has` / `follows` /
//! `precedes`) and composite (`all` / `any` / `not` / `matches`) algebra,
//! plus `stopBy` / `field` and utility-rule reuse, across TS and Rust.

use harn_rules::{CompiledRule, Rule};

fn run(toml: &str, source: &str) -> Vec<String> {
    let rule = Rule::from_toml_str(toml).expect("rule parses");
    let compiled = CompiledRule::compile(&rule).expect("rule compiles");
    compiled
        .run(source)
        .expect("runs")
        .into_iter()
        .map(|m| m.text)
        .collect()
}

#[test]
fn acceptance_inside_function_body_not_inside_try() {
    // "a `let` binding whose initializer is `$SRC?.$KEY ?? $DEF`, inside a
    // function body, not inside a try".
    let toml = r#"
        id = "let-default-in-fn-not-try"
        language = "typescript"
        [rule]
        pattern = "let $NAME = $SRC?.$KEY ?? $DEF"
        [rule.inside]
        kind = "statement_block"
        stopBy = "end"
        [rule.not.inside]
        kind = "try_statement"
        stopBy = "end"
    "#;
    let source = "\
function ok() {
  let a = cfg?.x ?? 1;
}
function bad() {
  try {
    let b = cfg?.y ?? 2;
  } catch (e) {}
}
let c = cfg?.z ?? 3;
";
    let matches = run(toml, source);
    // Only `a` qualifies: `b` is inside a try, `c` is not inside a function.
    assert_eq!(matches.len(), 1, "matches: {matches:?}");
    assert!(matches[0].contains("let a = cfg?.x ?? 1"));
}

#[test]
fn has_descendant() {
    // A function that contains a `throw` statement.
    let toml = r#"
        id = "fn-that-throws"
        language = "typescript"
        [rule]
        kind = "function_declaration"
        [rule.has]
        kind = "throw_statement"
        stopBy = "end"
    "#;
    let source = "\
function risky() { throw new Error('x'); }
function safe() { return 1; }
";
    let matches = run(toml, source);
    assert_eq!(matches.len(), 1, "matches: {matches:?}");
    assert!(matches[0].contains("risky"));
}

#[test]
fn follows_and_precedes() {
    // `follows` / `precedes` are sibling-relative, so match at the statement
    // level: an expression statement that follows a `let` declaration.
    let source = "\
function f() {
  let x = 1;
  useIt();
}
function g() {
  useIt();
}
";
    let follows = r#"
        id = "stmt-after-decl"
        language = "typescript"
        [rule]
        kind = "expression_statement"
        [rule.follows]
        kind = "lexical_declaration"
    "#;
    // Only the `useIt();` statement that follows `let x = 1;` qualifies.
    let m = run(follows, source);
    assert_eq!(m.len(), 1, "follows matches: {m:?}");
    assert!(m[0].contains("useIt()"));

    // The mirror image: a declaration that precedes an expression statement.
    let precedes = r#"
        id = "decl-before-stmt"
        language = "typescript"
        [rule]
        kind = "lexical_declaration"
        [rule.precedes]
        kind = "expression_statement"
    "#;
    let m = run(precedes, source);
    assert_eq!(m.len(), 1, "precedes matches: {m:?}");
    assert!(m[0].contains("let x = 1"));
}

#[test]
fn composite_any_and_all() {
    // `any`: match a call to either `foo` or `bar`.
    let any_toml = r#"
        id = "foo-or-bar"
        language = "typescript"
        [rule]
        any = [
          { pattern = "foo()" },
          { pattern = "bar()" },
        ]
    "#;
    let matches = run(any_toml, "foo();\nbaz();\nbar();\n");
    assert_eq!(matches.len(), 2, "matches: {matches:?}");

    // `all`: a call expression that is also inside a function.
    let all_toml = r#"
        id = "call-in-fn"
        language = "typescript"
        [rule]
        all = [
          { kind = "call_expression" },
          { inside = { kind = "statement_block", stopBy = "end" } },
        ]
    "#;
    let matches = run(all_toml, "function f() { go(); }\ntopLevel();\n");
    assert_eq!(matches.len(), 1, "matches: {matches:?}");
    assert!(matches[0].contains("go()"));
}

#[test]
fn utility_rule_referenced_by_matches() {
    // A util rule referenced from `matches`; the same util reused twice
    // resolves once (compiled into the rule's util map).
    let toml = r#"
        id = "uses-util"
        language = "typescript"
        [rule]
        all = [
          { matches = "is-call" },
          { inside = { kind = "statement_block", stopBy = "end" } },
        ]
        [utils.is-call]
        kind = "call_expression"
    "#;
    let matches = run(toml, "function f() { go(); }\nouter();\n");
    assert_eq!(matches.len(), 1, "matches: {matches:?}");
    assert!(matches[0].contains("go()"));
}

#[test]
fn stop_by_neighbor_vs_end() {
    // `neighbor` only checks the direct parent; `end` walks all ancestors.
    let source = "function f() { { deep(); } }\n";

    let neighbor = r#"
        id = "n"
        language = "typescript"
        [rule]
        pattern = "deep()"
        [rule.inside]
        kind = "function_declaration"
        stopBy = "neighbor"
    "#;
    // `deep()`'s direct ancestors are nested blocks, not the function — so
    // neighbor-scoped `inside` finds nothing.
    assert_eq!(run(neighbor, source).len(), 0);

    let end = r#"
        id = "e"
        language = "typescript"
        [rule]
        pattern = "deep()"
        [rule.inside]
        kind = "function_declaration"
        stopBy = "end"
    "#;
    assert_eq!(run(end, source).len(), 1);
}

#[test]
fn field_restricts_the_relation() {
    // `field = "body"` restricts the `inside` relation to the function's
    // body slot: the function body block matches, a nested block does not.
    let toml = r#"
        id = "fn-body-block"
        language = "typescript"
        [rule]
        kind = "statement_block"
        [rule.inside]
        kind = "function_declaration"
        field = "body"
        stopBy = "neighbor"
    "#;
    let matches = run(toml, "function f() { { nested(); } }\n");
    assert_eq!(matches.len(), 1, "matches: {matches:?}");
    // The matched block is the function body (it contains the nested block).
    assert!(matches[0].contains("nested()"));
}

#[test]
fn relational_in_rust_grammar() {
    // Second grammar: a `let` declaration inside a function body.
    let toml = r#"
        id = "rust-let-in-fn"
        language = "rust"
        [rule]
        pattern = "let $N = $V;"
        [rule.inside]
        kind = "function_item"
        stopBy = "end"
    "#;
    let source = "\
fn f() {
    let x = compute();
}
const TOP: i32 = 1;
";
    let matches = run(toml, source);
    assert_eq!(matches.len(), 1, "matches: {matches:?}");
    assert!(matches[0].contains("let x = compute()"));
}

//! `mutable-never-reassigned` — warning and `let → const` autofix.

use super::*;

#[test]
fn test_mutable_never_reassigned() {
    let diags = lint_source(
        r"
pipeline default(task) {
let x = 1
log(x)
}
",
    );
    assert!(
        has_rule(&diags, "mutable-never-reassigned"),
        "expected mutable-never-reassigned warning, got: {diags:?}"
    );
}

#[test]
fn test_mutable_reassigned_ok() {
    let diags = lint_source(
        r"
pipeline default(task) {
let x = 1
x = 2
log(x)
}
",
    );
    assert!(
        !has_rule(&diags, "mutable-never-reassigned"),
        "reassigned let should not trigger mutable-never-reassigned: {diags:?}"
    );
}

#[test]
fn test_fix_mutable_never_reassigned() {
    let source = "pipeline default(task) {\n  let x = 10\n  log(x)\n}";
    let diags = lint_source(source);
    let fix = get_fix(&diags, "mutable-never-reassigned");
    assert!(fix.is_some(), "expected fix for mutable-never-reassigned");
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("const x = 10"),
        "expected let→const, got: {result}"
    );
    assert!(
        !result.contains("let x"),
        "let should be replaced, got: {result}"
    );
}

#[test]
fn test_compound_assignment_counts_as_reassignment() {
    // `x += 1` is a reassignment: tightening to `const` would break it.
    let diags = lint_source(
        r"
pipeline default(task) {
let x = 1
x += 1
log(x)
}
",
    );
    assert!(
        !has_rule(&diags, "mutable-never-reassigned"),
        "compound-assigned let must not be flagged: {diags:?}"
    );
}

#[test]
fn test_index_and_member_reassignment_counts() {
    // `xs[0] = …` and `o.a = …` mutate the root binding; it must stay `let`.
    let index = lint_source(
        r"
pipeline default(task) {
let xs = [1]
xs[0] = 2
log(xs)
}
",
    );
    assert!(
        !has_rule(&index, "mutable-never-reassigned"),
        "index-reassigned let must not be flagged: {index:?}"
    );
    let member = lint_source(
        r"
pipeline default(task) {
let o = {a: 1}
o.a = 2
log(o)
}
",
    );
    assert!(
        !has_rule(&member, "mutable-never-reassigned"),
        "member-reassigned let must not be flagged: {member:?}"
    );
}

#[test]
fn test_destructuring_binding_not_tightened() {
    // A destructuring `let` governs every name it binds. Here `a` is
    // reassigned, so the binding cannot become `const {a, b}` (that would
    // freeze `a`). The lint must not fire on any field of a destructuring
    // pattern — an all-fields soundness check is deferred (see #4269).
    let diags = lint_source(
        r"
pipeline default(task) {
const src = {a: 1, b: 2}
let {a, b} = src
a = 9
log(a)
log(b)
}
",
    );
    assert!(
        !has_rule(&diags, "mutable-never-reassigned"),
        "destructuring binding must not be tightened: {diags:?}"
    );
}

#[test]
fn test_const_binding_is_idempotent() {
    // Re-linting an already-tightened `const` produces no further diagnostic,
    // so `harn fix` converges in one pass.
    let diags = lint_source(
        r"
pipeline default(task) {
const x = 10
log(x)
}
",
    );
    assert!(
        !has_rule(&diags, "mutable-never-reassigned"),
        "const binding must not be flagged: {diags:?}"
    );
}

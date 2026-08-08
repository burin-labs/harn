//! Static rejection of out-of-set literal arguments to homogeneous
//! string/int-literal union parameters — the shape the VM lowers to an `enum`
//! schema and enforces at runtime. A value known at check time is checked at
//! check time; runtime-valued data keeps the gradual concession.

use super::*;

const PHASE: &str = r#"type Phase = "submit" | "status" | "cancel" | "download"
pub fn use_phase(p: Phase) -> string { return p }
"#;

#[test]
fn rejects_string_literal_not_in_union() {
    let errs = errors(&format!(
        "{PHASE}pub fn caller() -> string {{ return use_phase(\"prepare\") }}"
    ));
    assert_eq!(errs.len(), 1, "expected exactly one error, got: {errs:?}");
    assert!(
        errs[0].contains("\"prepare\"") && errs[0].contains("not a permitted value"),
        "message should name the offending literal: {errs:?}"
    );
}

#[test]
fn accepts_string_literal_in_union() {
    let errs = errors(&format!(
        "{PHASE}pub fn caller() -> string {{ return use_phase(\"cancel\") }}"
    ));
    assert!(errs.is_empty(), "a member literal must pass: {errs:?}");
}

#[test]
fn accepts_inline_union_member_without_alias() {
    let errs = errors(
        r#"pub fn f(p: "a" | "b") -> string { return p }
pub fn g() -> string { return f("a") }"#,
    );
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn rejects_inline_union_non_member() {
    let errs = errors(
        r#"pub fn f(p: "a" | "b") -> string { return p }
pub fn g() -> string { return f("c") }"#,
    );
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(errs[0].contains("\"c\""), "{errs:?}");
}

#[test]
fn rejects_int_literal_not_in_union() {
    let errs = errors(
        r"type Level = 1 | 2 | 3
pub fn at(l: Level) -> int { return l }
pub fn g() -> int { return at(4) }",
    );
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(errs[0].contains('4'), "{errs:?}");
}

#[test]
fn accepts_int_literal_in_union() {
    let errs = errors(
        r"type Level = 1 | 2 | 3
pub fn at(l: Level) -> int { return l }
pub fn g() -> int { return at(2) }",
    );
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn preserves_gradual_concession_for_runtime_valued_string() {
    // A `string`-typed *variable* (value unknown at check time) still flows
    // into a literal-union slot — this is the deliberate gradual concession
    // that runtime schema validation backstops. The static check must not
    // regress it, or existing discriminated-union call sites would break.
    let errs = errors(&format!(
        "{PHASE}pub fn caller(raw: string) -> string {{ return use_phase(raw) }}"
    ));
    assert!(
        errs.is_empty(),
        "runtime-valued string must keep flowing into a literal union: {errs:?}"
    );
}

#[test]
fn does_not_fire_on_mixed_union_with_open_string() {
    // `"a" | "b" | string` accepts any string at runtime (the VM does not
    // build an enum schema for a mixed union), so a non-listed literal is
    // legal and must not be flagged.
    let errs = errors(
        r#"pub fn f(p: "a" | "b" | string) -> string { return p }
pub fn g() -> string { return f("c") }"#,
    );
    assert!(errs.is_empty(), "mixed union must not fire: {errs:?}");
}

#[test]
fn does_not_fire_on_optional_literal_union() {
    // `Phase | nil` is a mixed union (nil member), so the VM does not build a
    // pure enum; leave it to runtime rather than risk a static false positive.
    let errs = errors(&format!(
        "{PHASE}pub fn caller() -> string? {{ return use_phase_opt(\"prepare\") }}\npub fn use_phase_opt(p: Phase?) -> string? {{ return p }}"
    ));
    assert!(
        errs.iter().all(|e| !e.contains("not a permitted value")),
        "optional literal union must not trigger the literal-union check: {errs:?}"
    );
}

#[test]
fn resolves_nested_alias_union() {
    // `type Wide = Phase | "extra"` — the check must resolve and flatten
    // nested alias unions before deciding membership.
    let errs = errors(&format!(
        "{PHASE}type Wide = Phase | \"extra\"\npub fn use_wide(w: Wide) -> string {{ return w }}\npub fn g() -> string {{ return use_wide(\"nope\") }}"
    ));
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(errs[0].contains("\"nope\""), "{errs:?}");
    // ...and a member of the flattened set still passes.
    let ok = errors(&format!(
        "{PHASE}type Wide = Phase | \"extra\"\npub fn use_wide(w: Wide) -> string {{ return w }}\npub fn g() -> string {{ return use_wide(\"extra\") }}"
    ));
    assert!(ok.is_empty(), "{ok:?}");
}

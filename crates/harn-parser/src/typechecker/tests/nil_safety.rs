//! Member-access nil safety: property (`obj.field`), subscript
//! (`obj[key]`), and method-call (`obj.m()`) receivers are all held to
//! the same standard. A statically-`nil` or `T | nil` receiver is an
//! error; an `unknown` receiver is a warning; `any` opts out; and the
//! optional forms (`?.`, `?[]`, `?.()`) suppress the nil diagnostic.

use super::*;

fn has(msgs: &[String], needle: &str) -> bool {
    msgs.iter().any(|m| m.contains(needle))
}

// --- subscript parity ---------------------------------------------------

#[test]
fn subscript_on_nil_is_error() {
    let errs = errors("pipeline t(task) { let x: nil = nil\nlog(x[\"k\"]) }");
    assert!(
        has(&errs, "cannot access an index on `nil`"),
        "got: {errs:?}"
    );
}

#[test]
fn subscript_on_nilable_is_error() {
    let errs = errors("pipeline t(task) { let xs: list | nil = nil\nlog(xs[0]) }");
    assert!(
        has(&errs, "cannot access an index on nilable type"),
        "got: {errs:?}"
    );
}

#[test]
fn subscript_on_unknown_is_warning() {
    let warns = warnings("pipeline t(task) { let u: unknown = task\nlog(u[\"k\"]) }");
    assert!(
        has(&warns, "subscript access on an `unknown` value"),
        "got: {warns:?}"
    );
}

#[test]
fn subscript_on_any_is_silent() {
    let errs = errors("pipeline t(task) { let x: any = task\nlog(x[\"k\"]) }");
    let warns = warnings("pipeline t(task) { let x: any = task\nlog(x[\"k\"]) }");
    assert!(
        !has(&errs, "index") && !has(&warns, "subscript"),
        "any should opt out; errs={errs:?} warns={warns:?}"
    );
}

#[test]
fn optional_subscript_on_nil_is_allowed() {
    let errs = errors("pipeline t(task) { let x: list | nil = nil\nlog(x?[0]) }");
    assert!(
        !has(&errs, "cannot access an index"),
        "?[ ] should suppress the nil error; got: {errs:?}"
    );
}

#[test]
fn subscript_on_concrete_list_is_silent() {
    let errs = errors("pipeline t(task) { let xs: list = []\nlog(xs[0]) }");
    let warns = warnings("pipeline t(task) { let xs: list = []\nlog(xs[0]) }");
    assert!(
        errs.is_empty() && warns.is_empty(),
        "errs={errs:?} warns={warns:?}"
    );
}

// --- method-call parity -------------------------------------------------

#[test]
fn method_on_nil_is_error() {
    let errs = errors("pipeline t(task) { let x: nil = nil\nlog(x.foo()) }");
    assert!(
        has(&errs, "cannot access method `foo` on `nil`"),
        "got: {errs:?}"
    );
}

#[test]
fn method_on_nilable_is_error() {
    let errs = errors("pipeline t(task) { let s: string | nil = nil\nlog(s.upper()) }");
    assert!(
        has(&errs, "cannot access method `upper` on nilable type"),
        "got: {errs:?}"
    );
}

#[test]
fn method_on_unknown_is_warning() {
    let warns = warnings("pipeline t(task) { let u: unknown = task\nlog(u.run()) }");
    assert!(
        has(&warns, "method call `.run()` on an `unknown` value"),
        "got: {warns:?}"
    );
}

#[test]
fn method_on_any_is_silent() {
    let errs = errors("pipeline t(task) { let x: any = task\nlog(x.run()) }");
    let warns = warnings("pipeline t(task) { let x: any = task\nlog(x.run()) }");
    assert!(
        !has(&errs, "method") && !has(&warns, "method call"),
        "any should opt out; errs={errs:?} warns={warns:?}"
    );
}

#[test]
fn optional_method_on_nilable_is_allowed() {
    let errs = errors("pipeline t(task) { let s: string | nil = nil\nlog(s?.upper()) }");
    assert!(
        !has(&errs, "cannot access method"),
        "?.m() should suppress the nil error; got: {errs:?}"
    );
}

#[test]
fn method_on_concrete_string_is_silent() {
    let errs = errors("pipeline t(task) { let s: string = \"x\"\nlog(s.upper()) }");
    assert!(!has(&errs, "cannot access method"), "got: {errs:?}");
}

// --- property access still behaves (regression after the refactor) ------

#[test]
fn property_on_nil_still_errors() {
    let errs = errors("pipeline t(task) { let x: nil = nil\nlog(x.foo) }");
    assert!(
        has(&errs, "cannot access property `foo` on `nil`"),
        "got: {errs:?}"
    );
}

#[test]
fn property_on_unknown_still_warns() {
    let warns = warnings("pipeline t(task) { let u: unknown = task\nlog(u.field) }");
    assert!(
        has(&warns, "property access `.field` on an `unknown` value"),
        "got: {warns:?}"
    );
}

// --- narrowing that keeps the strict rule usable ------------------------

#[test]
fn optional_chain_guard_narrows_base() {
    // `o?.a != nil` proves `o` is non-nil on the truthy branch, so a plain
    // `o.a` read inside the guard must not fire the nilable-receiver error.
    let errs = errors(
        "pipeline t(task) { let o: {a?: string} | nil = nil\n\
         if o?.a != nil {\nlog(o.a)\n} }",
    );
    assert!(
        !has(&errs, "cannot access property"),
        "o?.a != nil should narrow o to non-nil; got: {errs:?}"
    );
}

#[test]
fn optional_chain_guard_does_not_narrow_wrong_branch() {
    // `o?.a == nil` is satisfiable with `o` itself nil, so the truthy branch
    // must NOT be narrowed — the strict receiver error still fires.
    let errs = errors(
        "pipeline t(task) { let o: {a?: string} | nil = nil\n\
         if o?.a == nil {\nlog(o.a)\n} }",
    );
    assert!(
        has(&errs, "cannot access property"),
        "o?.a == nil must not narrow o; got: {errs:?}"
    );
}

#[test]
fn coalesce_strips_nil_through_named_alias() {
    // `??` drops the nil arm even when the left operand's type is a named
    // alias that expands to a nilable union (the inline-union case already
    // worked; this is the alias parity fix).
    let errs = errors(
        "type Opts = {a?: string} | nil\n\
         pipeline t(task) { let o: Opts = nil\n\
         let p = o ?? {}\nlog(p.a) }",
    );
    assert!(
        !has(&errs, "cannot access property"),
        "o ?? {{}} should be non-nil even for an aliased nilable type; got: {errs:?}"
    );
}

// --- the loose dict-literal idiom stays loose ---------------------------

#[test]
fn dict_literal_subscript_stays_loose() {
    let errs = errors("pipeline t(task) { let d = {a: 1}\nlog(d[\"b\"]) }");
    let warns = warnings("pipeline t(task) { let d = {a: 1}\nlog(d[\"b\"]) }");
    assert!(
        !has(&errs, "index") && !has(&warns, "subscript"),
        "ambient dict idiom should stay loose; errs={errs:?} warns={warns:?}"
    );
}

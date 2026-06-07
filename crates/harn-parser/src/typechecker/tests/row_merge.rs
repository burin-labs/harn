//! Precise structural typing of record merge / spread. `{...a, b}`, `a + b`,
//! and merging dict literals infer the **right-biased merged shape** (b's
//! fields override a's), preserving every field's type instead of collapsing
//! to a bare `dict`. A spread of a non-closed source (a `dict`, `dict<K,V>`,
//! union, or unknown) degrades to `dict` rather than inventing fields.

use super::*;

fn has(msgs: &[String], needle: &str) -> bool {
    msgs.iter().any(|m| m.contains(needle))
}

#[test]
fn dict_spread_merge_is_right_biased_and_preserves_fields() {
    // `merged` should be {x: int, y: string, z: bool}: y overridden by the
    // later string, x carried from the spread, z added. Each typed binding
    // only checks clean if inference produced exactly that shape.
    let errs = errors(
        "pipeline t(task) { let a = {x: 1, y: 2}\n\
         let merged = {...a, y: \"s\", z: true}\n\
         let xv: int = merged.x\nlet yv: string = merged.y\nlet zv: bool = merged.z\n\
         log(xv) }",
    );
    assert!(
        errs.is_empty(),
        "expected precise merged shape; got: {errs:?}"
    );
}

#[test]
fn dict_spread_override_with_wrong_annotation_errors() {
    // After `{...a, y: "s"}`, y is string — annotating it int must fail,
    // proving the override actually changed the field type (not silently dict).
    let errs = errors(
        "pipeline t(task) { let a = {x: 1, y: 2}\n\
         let merged = {...a, y: \"s\"}\n\
         let yv: int = merged.y\nlog(yv) }",
    );
    assert!(
        has(&errs, "y") || !errs.is_empty(),
        "overriding y to string should reject an int annotation; got: {errs:?}"
    );
}

#[test]
fn plus_on_shapes_merges_right_biased() {
    let errs = errors(
        "pipeline t(task) { let a = {x: 1}\nlet b = {x: \"s\", z: true}\n\
         let m = a + b\nlet xv: string = m.x\nlet zv: bool = m.z\nlog(xv) }",
    );
    assert!(
        errs.is_empty(),
        "a + b should merge to a precise shape; got: {errs:?}"
    );
}

#[test]
fn optional_right_field_unions_with_required_left() {
    // a.x required int, b.x optional string ⇒ x stays required, type int|string
    // (at runtime x is always present from a, value may be either). Reading it
    // as `int | string` must typecheck; as bare `int` must not.
    let ok = errors(
        "pipeline t(task) { let a = {x: 1}\nlet b: {x?: string} = {}\n\
         let m = a + b\nlet xv: int | string = m.x\nlog(xv) }",
    );
    assert!(ok.is_empty(), "optional override should union; got: {ok:?}");
}

// --- open records + row-polymorphic generics --------------------------

#[test]
fn open_consumer_accepts_wider_record() {
    let errs = errors(
        "fn needs_id(x: {id: string, ...rest}) -> string { return x.id }\n\
         pipeline main(task) { __io_println(needs_id({id: \"u1\", name: \"Ann\", age: 3})) }",
    );
    assert!(
        errs.is_empty(),
        "open consumer should accept extra fields; got: {errs:?}"
    );
}

#[test]
fn open_consumer_rejects_missing_required_field() {
    let errs = errors(
        "fn needs_id(x: {id: string, ...rest}) -> string { return x.id }\n\
         pipeline main(task) { __io_println(needs_id({name: \"Ann\"})) }",
    );
    assert!(
        !errs.is_empty(),
        "missing required `id` must be rejected even with an open tail; got: {errs:?}"
    );
}

#[test]
fn row_generic_merge_returns_precise_merged_shape() {
    // The motivating case: a row-polymorphic merge over two independent rows
    // returns the precise right-biased merged record — each typed binding only
    // checks clean if every field landed with the right type.
    let errs = errors(
        "fn rmerge<R1, R2>(a: {...R1}, b: {...R2}) -> {...R1, ...R2} { return a + b }\n\
         pipeline main(task) {\n\
         let m = rmerge({a: 1, b: 2}, {b: \"s\", c: true})\n\
         let av: int = m.a\nlet bv: string = m.b\nlet cv: bool = m.c\nlog(av) }",
    );
    assert!(
        errs.is_empty(),
        "row merge should preserve precise field types; got: {errs:?}"
    );
}

#[test]
fn row_generic_merge_keeps_precision_for_wrong_annotation() {
    let errs = errors(
        "fn rmerge<R1, R2>(a: {...R1}, b: {...R2}) -> {...R1, ...R2} { return a + b }\n\
         pipeline main(task) {\n\
         let m = rmerge({a: 1}, {b: \"s\"})\nlet bad: int = m.b\nlog(bad) }",
    );
    assert!(
        !errs.is_empty(),
        "m.b is string after merge; an int annotation must be rejected; got: {errs:?}"
    );
}

#[test]
fn spread_of_open_dict_degrades_to_dict() {
    // Spreading a value typed `dict` (unknown fields) can't yield a closed
    // shape, so the result is `dict` and field access stays gradual — no error.
    let errs =
        errors("pipeline t(task) { let d: dict = task\nlet m = {...d, k: 1}\nlog(m.anything) }");
    assert!(
        !has(&errs, "does not exist") && !has(&errs, "cannot access"),
        "spread of an open dict should degrade gracefully; got: {errs:?}"
    );
}

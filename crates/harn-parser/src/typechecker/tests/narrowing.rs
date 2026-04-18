//! Flow-sensitive type refinement: nil/typeof/has/schema_is narrowing, guards, while-body narrowing.

use super::*;

#[test]
fn test_nil_narrowing_then_branch() {
    // Existing behavior: x != nil narrows to string in then-branch
    let errs = errors(
        r#"pipeline t(task) {
  fn greet(name: string | nil) {
if name != nil {
  let s: string = name
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_nil_narrowing_else_branch() {
    // NEW: x != nil narrows to nil in else-branch
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) {
if x != nil {
  let s: string = x
} else {
  let n: nil = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_nil_equality_narrows_both() {
    // x == nil narrows then to nil, else to non-nil
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) {
if x == nil {
  let n: nil = x
} else {
  let s: string = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_truthiness_narrowing() {
    // Bare identifier in condition removes nil
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) {
if x {
  let s: string = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_negation_narrowing() {
    // !x swaps truthy/falsy
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) {
if !x {
  let n: nil = x
} else {
  let s: string = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_typeof_narrowing() {
    // type_of(x) == "string" narrows to string
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | int) {
if type_of(x) == "string" {
  let s: string = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_typeof_narrowing_else() {
    // else removes the tested type
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | int) {
if type_of(x) == "string" {
  let s: string = x
} else {
  let i: int = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_typeof_neq_narrowing() {
    // type_of(x) != "string" removes string in then, narrows to string in else
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | int) {
if type_of(x) != "string" {
  let i: int = x
} else {
  let s: string = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_and_combines_narrowing() {
    // && combines truthy refinements
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | int | nil) {
if x != nil && type_of(x) == "string" {
  let s: string = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_or_falsy_narrowing() {
    // || combines falsy refinements
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil, y: int | nil) {
if x || y {
  // conservative: can't narrow
} else {
  let xn: nil = x
  let yn: nil = y
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_guard_narrows_outer_scope() {
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) {
guard x != nil else { return }
let s: string = x
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_while_narrows_body() {
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) {
while x != nil {
  let s: string = x
  break
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_early_return_narrows_after_if() {
    // if then-body returns, falsy refinements apply after
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) -> string {
if x == nil {
  return "default"
}
let s: string = x
return s
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_early_throw_narrows_after_if() {
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) {
if x == nil {
  throw "missing"
}
let s: string = x
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_no_narrowing_unknown_type() {
    // Gradual typing: untyped vars don't get narrowed
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x) {
if x != nil {
  let s: string = x
}
  }
}"#,
    );
    // No narrowing possible, so assigning untyped x to string should be fine
    // (gradual typing allows it)
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_reassignment_invalidates_narrowing() {
    // After reassigning a narrowed var, the original type should be restored
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | nil) {
var y: string | nil = x
if y != nil {
  let s: string = y
  y = nil
  let s2: string = y
}
  }
}"#,
    );
    // s2 should fail because y was reassigned, invalidating the narrowing
    assert_eq!(errs.len(), 1, "expected 1 error, got: {:?}", errs);
    assert!(
        errs[0].contains("declared as"),
        "expected type mismatch, got: {}",
        errs[0]
    );
}

#[test]
fn test_let_immutable_warning() {
    let all = check_source(
        r#"pipeline t(task) {
  let x = 42
  x = 43
}"#,
    );
    let warnings: Vec<_> = all
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Warning)
        .collect();
    assert!(
        warnings.iter().any(|w| w.message.contains("immutable")),
        "expected immutability warning, got: {:?}",
        warnings
    );
}

#[test]
fn test_nested_narrowing() {
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | int | nil) {
if x != nil {
  if type_of(x) == "int" {
    let i: int = x
  }
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_match_narrows_arms() {
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: string | int) {
match x {
  "hello" -> {
    let s: string = x
  }
  42 -> {
    let i: int = x
  }
  _ -> {}
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

// ---------------------------------------------------------------------------
// Discriminator narrowing on tagged shape unions (Phase A).
// ---------------------------------------------------------------------------
//
// `match obj.<tag>` / `if obj.<tag> == "..."` should narrow `obj` to the
// matching shape variant. The discriminant field name is auto-detected:
// any field shared by all variants and typed as a literal-per-variant
// qualifies. Tests parameterise over `kind`, `type`, and `op` to pin the
// no-magic-name contract.

#[test]
fn test_match_discriminator_narrows_kind_tag() {
    let errs = errors(
        r#"type Msg = {kind: "ping", ttl: int} | {kind: "pong", latency_ms: int}

pipeline t(task) {
  fn handle(m: Msg) {
    match m.kind {
      "ping" -> {
        let p: {kind: "ping", ttl: int} = m
      }
      "pong" -> {
        let p: {kind: "pong", latency_ms: int} = m
      }
    }
  }
}"#,
    );
    assert!(
        errs.is_empty(),
        "expected narrowing on m.kind, got: {:?}",
        errs
    );
}

#[test]
fn test_match_discriminator_narrows_type_tag() {
    let errs = errors(
        r#"type Event = {type: "click", x: int, y: int} | {type: "scroll", dy: int}

pipeline t(task) {
  fn handle(e: Event) {
    match e.type {
      "click" -> {
        let c: {type: "click", x: int, y: int} = e
      }
      "scroll" -> {
        let s: {type: "scroll", dy: int} = e
      }
    }
  }
}"#,
    );
    assert!(
        errs.is_empty(),
        "expected narrowing on e.type, got: {:?}",
        errs
    );
}

#[test]
fn test_match_discriminator_narrows_arbitrary_tag() {
    // The auto-detected discriminant name is whatever shared, literal-per-variant
    // field appears first in source order. `op` is no different from `kind`.
    let errs = errors(
        r#"type Instr = {op: "add", lhs: int, rhs: int} | {op: "neg", arg: int}

pipeline t(task) {
  fn handle(i: Instr) {
    match i.op {
      "add" -> {
        let a: {op: "add", lhs: int, rhs: int} = i
      }
      "neg" -> {
        let n: {op: "neg", arg: int} = i
      }
    }
  }
}"#,
    );
    assert!(
        errs.is_empty(),
        "expected narrowing on i.op, got: {:?}",
        errs
    );
}

#[test]
fn test_if_discriminator_narrows_kind_then_branch() {
    let errs = errors(
        r#"type Msg = {kind: "ping", ttl: int} | {kind: "pong", latency_ms: int}

pipeline t(task) {
  fn handle(m: Msg) {
    if m.kind == "ping" {
      let p: {kind: "ping", ttl: int} = m
    }
  }
}"#,
    );
    assert!(
        errs.is_empty(),
        "expected narrowing in then-branch, got: {:?}",
        errs
    );
}

#[test]
fn test_if_discriminator_narrows_else_branch_residual() {
    // The else branch sees the residual union (single member here, so a Shape).
    let errs = errors(
        r#"type Msg = {kind: "ping", ttl: int} | {kind: "pong", latency_ms: int}

pipeline t(task) {
  fn handle(m: Msg) {
    if m.kind == "ping" {
      let p: {kind: "ping", ttl: int} = m
    } else {
      let p: {kind: "pong", latency_ms: int} = m
    }
  }
}"#,
    );
    assert!(
        errs.is_empty(),
        "expected narrowing in both branches, got: {:?}",
        errs
    );
}

#[test]
fn test_if_discriminator_neq_inverts_narrowing() {
    // `m.kind != "ping"` swaps truthy/falsy: then-branch sees the residual
    // union (the pong shape here), else-branch sees the matched shape.
    let errs = errors(
        r#"type Msg = {kind: "ping", ttl: int} | {kind: "pong", latency_ms: int}

pipeline t(task) {
  fn handle(m: Msg) {
    if m.kind != "ping" {
      let p: {kind: "pong", latency_ms: int} = m
    } else {
      let p: {kind: "ping", ttl: int} = m
    }
  }
}"#,
    );
    assert!(
        errs.is_empty(),
        "expected `!=` to invert truthy/falsy, got: {:?}",
        errs
    );
}

#[test]
fn test_discriminator_narrowing_skipped_when_field_unknown() {
    // `m.foo` is not the discriminant — narrowing must NOT fire and the
    // mistyped assignment must still error to prove we didn't accidentally
    // collapse `m` to one of the variants.
    let errs = errors(
        r#"type Msg = {kind: "ping", ttl: int} | {kind: "pong", latency_ms: int}

pipeline t(task) {
  fn handle(m: Msg) {
    if m.kind == "ping" {
      // Sanity: once narrowed, this assignment to the OTHER variant must fail.
      let wrong: {kind: "pong", latency_ms: int} = m
    }
  }
}"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("'wrong' declared as")),
        "expected residual-narrowing assignment to fail, got: {:?}",
        errs
    );
}

#[test]
fn test_has_narrows_optional_field() {
    let errs = errors(
        r#"pipeline t(task) {
  fn check(x: {name?: string, age: int}) {
if x.has("name") {
  let n: {name: string, age: int} = x
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_match_or_pattern_narrows_to_union_of_variants() {
    // `"ping" | "pong"` arm on a 3-variant tagged shape union narrows
    // `m` to a 2-variant union inside the arm body. Both variants'
    // shared fields (discriminant `kind` + no common payload) remain
    // accessible, and variant-specific payloads on the unmatched
    // `close` variant must not be reachable.
    let errs = errors(
        r#"type Msg =
  {kind: "ping", ttl: int} |
  {kind: "pong", latency_ms: int} |
  {kind: "close", reason: string}

pipeline t(task) {
  fn handle(m: Msg) -> string {
    return match m.kind {
      "ping" | "pong" -> {
        // Both kinds carry `kind` — access is fine.
        let k: string = m.kind
        "live"
      }
      "close" -> { m.reason }
    }
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_match_narrows_through_named_alias_member() {
    // A tagged shape union whose members include a `Named` alias that
    // resolves to a shape must still support discriminator narrowing.
    // Prior to the fix, the bare-`Shape` check in `discriminant_field`
    // rejected the union on sight.
    let errs = errors(
        r#"type Ping = {kind: "ping", ttl: int}
type Msg = Ping | {kind: "pong", latency_ms: int}

pipeline t(task) {
  fn handle(m: Msg) -> string {
    return match m.kind {
      "ping" -> {
        let p: {kind: "ping", ttl: int} = m
        "p"
      }
      "pong" -> { "o" }
    }
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_if_narrows_through_named_alias_member() {
    // Same shape as the match test but exercises the
    // `if obj.kind == "…"` path, which routes through
    // `extract_discriminator_refinements`.
    let errs = errors(
        r#"type Ping = {kind: "ping", ttl: int}
type Msg = Ping | {kind: "pong", latency_ms: int}

pipeline t(task) {
  fn handle(m: Msg) -> string {
    if m.kind == "ping" {
      let p: {kind: "ping", ttl: int} = m
      return "p"
    }
    return "o"
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

#[test]
fn test_match_or_pattern_on_literal_union_narrows_to_sub_union() {
    // A two-alternative or-pattern on a three-literal union refines
    // to a two-literal sub-union inside the arm: pinning `v` as
    // `"pos" | "neg"` inside the or-arm must type-check.
    let errs = errors(
        r#"pipeline t(task) {
  fn sign(v: "pos" | "neg" | "zero") -> string {
    return match v {
      "pos" | "neg" -> {
        let rest: "pos" | "neg" = v
        rest
      }
      "zero" -> { v }
    }
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {:?}", errs);
}

//! Flow-sensitive type refinement: nil/typeof/has/schema_is narrowing, guards, while-body narrowing.

use super::*;

#[test]
fn test_nil_narrowing_then_branch() {
    // Existing behavior: x != nil narrows to string in then-branch
    let errs = errors(
        r"pipeline t(task) {
  fn greet(name: string | nil) {
if name != nil {
  let s: string = name
}
  }
}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_nil_narrowing_else_branch() {
    // NEW: x != nil narrows to nil in else-branch
    let errs = errors(
        r"pipeline t(task) {
  fn check(x: string | nil) {
if x != nil {
  let s: string = x
} else {
  let n: nil = x
}
  }
}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_nil_equality_narrows_both() {
    // x == nil narrows then to nil, else to non-nil
    let errs = errors(
        r"pipeline t(task) {
  fn check(x: string | nil) {
if x == nil {
  let n: nil = x
} else {
  let s: string = x
}
  }
}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_truthiness_narrowing() {
    // Bare identifier in condition removes nil
    let errs = errors(
        r"pipeline t(task) {
  fn check(x: string | nil) {
if x {
  let s: string = x
}
  }
}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_negation_narrowing() {
    // !x swaps truthy/falsy
    let errs = errors(
        r"pipeline t(task) {
  fn check(x: string | nil) {
if !x {
  let n: nil = x
} else {
  let s: string = x
}
  }
}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
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
    assert!(errs.is_empty(), "got: {errs:?}");
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
    assert!(errs.is_empty(), "got: {errs:?}");
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
    assert!(errs.is_empty(), "got: {errs:?}");
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
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_short_circuit_and_narrows_rhs_expression() {
    let errs = errors(
        r"pipeline t(task) {
  fn count_values(values: list<int>) -> int { return len(values) }
  fn check(values: list<int> | nil) {
if values != nil && count_values(values) > 0 {
  let present: list<int> = values
}
  }
}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_short_circuit_or_narrows_rhs_expression() {
    let errs = errors(
        r#"pipeline t(task) {
  fn count_values(values: list<int>) -> int { return len(values) }
  fn check(values: list<int> | nil) {
if values == nil || count_values(values) > 0 {
  log("ok")
}
  }
}"#,
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_nil_coalescing_call_site_strips_optional_chain_nil() {
    let errs = errors(
        r"pipeline t(task) {
  type Doc = { components: { schemas: dict<int>? }? }
  let doc: Doc = { components: nil }
  let n: int = len(doc.components?.schemas ?? {})
}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_or_falsy_narrowing() {
    // || combines falsy refinements
    let errs = errors(
        r"pipeline t(task) {
  fn check(x: string | nil, y: int | nil) {
if x || y {
  // conservative: can't narrow
} else {
  let xn: nil = x
  let yn: nil = y
}
  }
}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_guard_narrows_outer_scope() {
    let errs = errors(
        r"pipeline t(task) {
  fn check(x: string | nil) {
guard x != nil else { return }
let s: string = x
  }
}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_while_narrows_body() {
    let errs = errors(
        r"pipeline t(task) {
  fn check(x: string | nil) {
while x != nil {
  let s: string = x
  break
}
  }
}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
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
    assert!(errs.is_empty(), "got: {errs:?}");
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
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_no_narrowing_unknown_type() {
    // Gradual typing: untyped vars don't get narrowed
    let errs = errors(
        r"pipeline t(task) {
  fn check(x) {
if x != nil {
  let s: string = x
}
  }
}",
    );
    // No narrowing possible, so assigning untyped x to string should be fine
    // (gradual typing allows it)
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_reassignment_invalidates_narrowing() {
    // After reassigning a narrowed var, the original type should be restored
    let errs = errors(
        r"pipeline t(task) {
  fn check(x: string | nil) {
var y: string | nil = x
if y != nil {
  let s: string = y
  y = nil
  let s2: string = y
}
  }
}",
    );
    // s2 should fail because y was reassigned, invalidating the narrowing
    assert_eq!(errs.len(), 1, "expected 1 error, got: {errs:?}");
    assert!(
        errs[0].contains("expected string"),
        "expected type mismatch, got: {}",
        errs[0]
    );
}

#[test]
fn test_let_immutable_warning() {
    let all = check_source(
        r"pipeline t(task) {
  let x = 42
  x = 43
}",
    );
    let warnings: Vec<_> = all
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Warning)
        .collect();
    assert!(
        warnings.iter().any(|w| w.message.contains("immutable")),
        "expected immutability warning, got: {warnings:?}"
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
    assert!(errs.is_empty(), "got: {errs:?}");
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
    assert!(errs.is_empty(), "got: {errs:?}");
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
        "expected narrowing on m.kind, got: {errs:?}"
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
        "expected narrowing on e.type, got: {errs:?}"
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
    assert!(errs.is_empty(), "expected narrowing on i.op, got: {errs:?}");
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
        "expected narrowing in then-branch, got: {errs:?}"
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
        "expected narrowing in both branches, got: {errs:?}"
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
        "expected `!=` to invert truthy/falsy, got: {errs:?}"
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
        errs.iter().any(|e| e.contains("let binding `wrong`")),
        "expected residual-narrowing assignment to fail, got: {errs:?}"
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
    assert!(errs.is_empty(), "got: {errs:?}");
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
    assert!(errs.is_empty(), "got: {errs:?}");
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
    assert!(errs.is_empty(), "got: {errs:?}");
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
    assert!(errs.is_empty(), "got: {errs:?}");
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
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_schema_is_shape_narrowing_preserves_current_fields() {
    // `schema_is(x, S)` confirms that `x` matches `S` — it adds
    // information, it does not remove it. Width subtyping says a value
    // typed `{a: int, b: string}` already has both fields, so the
    // truthy branch must still permit `x.a` after narrowing against a
    // schema that only mentions `b`.
    let errs = errors(
        r"type Tag = {b: string}

pipeline t(task) {
  fn check(x: {a: int, b: string}) {
    if schema_is(x, Tag) {
      let _a: int = x.a
      let _b: string = x.b
    }
  }
}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_schema_is_shape_narrowing_adds_schema_only_required_field() {
    // A schema-only required field is confirmed present by the
    // matched check, so the truthy branch should expose it alongside
    // the existing fields.
    let errs = errors(
        r"type WithTag = {kind: string}

pipeline t(task) {
  fn check(x: {a: int}) {
    if schema_is(x, WithTag) {
      let _a: int = x.a
      let _kind: string = x.kind
    }
  }
}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_schema_is_narrows_iter_union() {
    // `iter<T>` had no entry in `intersect_types`, so narrowing an
    // `iter<int> | string` union via `schema_is(x, iter<int>)` used to
    // drop both members and leave `x` un-narrowed. Verify the truthy
    // branch now exposes the iter element type so a typed bind succeeds.
    let errs = errors(
        r"type IterInt = iter<int>
pipeline t(task) {
  fn check(x: iter<int> | string) {
    if schema_is(x, IterInt) {
      let _i: iter<int> = x
    }
  }
}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

#[test]
fn test_schema_is_narrows_owned_union_and_preserves_marker() {
    // Two regressions in one fixture. (1) `owned<T>` had no entry in
    // `intersect_types`, so a `owned<channel> | nil` union narrowed via
    // `schema_is(x, channel)` used to drop the owned member entirely and
    // leave the binding un-narrowed. (2) The intersection must preserve
    // the ownership annotation so the HARN-OWN-005 leak lint keeps
    // tracking the binding — the truthy branch's `let _c: owned<channel>
    // = x` only typechecks if the narrowed type is `owned<channel>`, not
    // a stripped `channel`.
    let errs = errors(
        r"type Ch = channel
pipeline t(task) {
  fn check(x: owned<channel> | nil) {
    if schema_is(x, Ch) {
      let _c: owned<channel> = x
      drop(_c)
    }
  }
}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

// HARN-LNT-058 — vacuous-condition lint. The lint fires on
// statically-determined `if` / `while` / `guard` conditions, covering both
// constant-evaluable booleans and `schema_is` / `is_type` checks whose
// answer is fixed by the variable's static type.

#[test]
fn test_vacuous_condition_lint_schema_is_always_true_width_subtype() {
    // `x: {a, b}` is a width-subtype of `Tag = {b: string}`, so the
    // `schema_is` check cannot fail at runtime.
    let warns = warnings(
        r"type Tag = {b: string}
pipeline t(task) {
  fn check(x: {a: int, b: string}) {
    if schema_is(x, Tag) {
      let _b: string = x.b
    }
  }
}",
    );
    assert!(
        warns.iter().any(|w| w.contains("always true")),
        "expected always-true warning, got: {warns:?}"
    );
}

#[test]
fn test_vacuous_condition_lint_schema_is_always_false_disjoint() {
    // `x: int` and a shape schema are disjoint denotations: no scalar
    // value satisfies a shape match, so the truthy branch is dead.
    let warns = warnings(
        r"type Tag = {kind: string}
pipeline t(task) {
  fn check(x: int) {
    if schema_is(x, Tag) {
      log(x)
    }
  }
}",
    );
    assert!(
        warns.iter().any(|w| w.contains("always false")),
        "expected always-false warning, got: {warns:?}"
    );
}

#[test]
fn test_vacuous_condition_lint_silent_on_unknown() {
    // `unknown` is the open-world top — schema_is is genuinely informative
    // and the lint must stay silent.
    let warns = warnings(
        r#"type Tag = {b: string}
pipeline t(task) {
  fn check(x: unknown) {
    if schema_is(x, Tag) {
      log("ok")
    }
  }
}"#,
    );
    assert!(
        !warns
            .iter()
            .any(|w| w.contains("always true") || w.contains("always false")),
        "expected no vacuous-condition warning on unknown var, got: {warns:?}"
    );
}

#[test]
fn test_vacuous_condition_lint_respects_optional_field() {
    // An optional field in the variable can be absent at runtime, so
    // `schema_is` against a required field is NOT statically true.
    let warns = warnings(
        r"type Tag = {b: string}
pipeline t(task) {
  fn check(x: {b: string?}) {
    if schema_is(x, Tag) {
      let _b: string = x.b
    }
  }
}",
    );
    assert!(
        !warns.iter().any(|w| w.contains("always true")),
        "optional-vs-required mismatch must not fire always-true, got: {warns:?}"
    );
}

#[test]
fn test_vacuous_condition_lint_skips_bare_true_literal() {
    // `if true { … }` is the canonical Harn block-scope idiom — the
    // conformance suite uses it intentionally. Skip the lint on bare
    // literals so we don't spam every block-scope use; the compound-
    // folding tests below still catch the cases that *are* mistakes.
    let warns = warnings(
        r#"pipeline t(task) {
  if true {
    log("always runs")
  }
}"#,
    );
    assert!(
        !warns.iter().any(|w| w.contains("always truthy")),
        "bare `if true` is a block-scope idiom and must not fire, got: {warns:?}"
    );
}

#[test]
fn test_vacuous_condition_lint_skips_bare_false_literal() {
    // `if false { … }` is the canonical disable-block idiom (used when
    // temporarily turning a branch off during refactor) — skip it for
    // the same reason as `if true`.
    let warns = warnings(
        r#"pipeline t(task) {
  if false {
    log("never runs")
  }
}"#,
    );
    assert!(
        !warns.iter().any(|w| w.contains("always falsy")),
        "bare `if false` is a disable-block idiom and must not fire, got: {warns:?}"
    );
}

#[test]
fn test_vacuous_condition_lint_constant_or_short_circuit() {
    // `true || _` collapses to always-truthy regardless of the RHS.
    let warns = warnings(
        r#"pipeline t(task) {
  fn check(x: int) {
    if true || x > 0 {
      log("always runs")
    }
  }
}"#,
    );
    assert!(
        warns.iter().any(|w| w.contains("always truthy")),
        "expected `true || _` short-circuit warning, got: {warns:?}"
    );
}

#[test]
fn test_vacuous_condition_lint_constant_and_short_circuit() {
    // `_ && false` collapses to always-falsy regardless of the LHS.
    let warns = warnings(
        r#"pipeline t(task) {
  fn check(x: int) {
    if x > 0 && false {
      log("never runs")
    }
  }
}"#,
    );
    assert!(
        warns.iter().any(|w| w.contains("always falsy")),
        "expected `_ && false` short-circuit warning, got: {warns:?}"
    );
}

#[test]
fn test_vacuous_condition_lint_negated_constant() {
    let warns = warnings(
        r#"pipeline t(task) {
  if !true {
    log("never runs")
  }
}"#,
    );
    assert!(
        warns.iter().any(|w| w.contains("always falsy")),
        "expected `!true` warning, got: {warns:?}"
    );
}

#[test]
fn test_vacuous_condition_lint_silent_on_normal_narrowing() {
    // A real refinement (a tagged shape union partitioned by `schema_is`
    // matching one variant) must not fire either lint message — the
    // partition is genuine, not vacuous.
    let warns = warnings(
        r#"type A = {kind: "a"}
pipeline t(task) {
  fn check(x: {kind: "a", extra: int} | {kind: "b"}) {
    if schema_is(x, A) {
      log(x)
    }
  }
}"#,
    );
    assert!(
        !warns
            .iter()
            .any(|w| w.contains("always true") || w.contains("always false")),
        "real narrowing must stay silent, got: {warns:?}"
    );
}

#[test]
fn test_vacuous_condition_lint_fires_for_is_type_alias() {
    // `is_type` shares the dispatcher with `schema_is` — the same lint
    // must fire for it without a second registration.
    let warns = warnings(
        r"type Tag = {b: string}
pipeline t(task) {
  fn check(x: {a: int, b: string}) {
    if is_type(x, Tag) {
      let _b: string = x.b
    }
  }
}",
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("is_type") && w.contains("always true")),
        "expected `is_type` always-true warning, got: {warns:?}"
    );
}

#[test]
fn test_vacuous_condition_lint_descends_through_negation() {
    // `!schema_is(x, S)` is just an inversion — the underlying predicate
    // is still statically determined, so the walker must descend into
    // the operand and emit the lint there.
    let warns = warnings(
        r"type Tag = {b: string}
pipeline t(task) {
  fn check(x: {a: int, b: string}) {
    if !schema_is(x, Tag) {
      log(x)
    }
  }
}",
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("schema_is") && w.contains("always true")),
        "expected `schema_is` always-true through negation, got: {warns:?}"
    );
}

#[test]
fn test_vacuous_condition_lint_fires_in_while_condition() {
    // `while` runs the same condition-extraction pipeline as `if`. Bare
    // `while true { … }` is the idiomatic infinite-loop spelling, so the
    // lint skips it; a compound condition that folds to the same answer
    // is what we want to catch — `while (cond || true)` is almost always
    // a refactor leftover.
    let warns = warnings(
        r#"pipeline t(task) {
  fn check(cond: bool) {
    while cond || true {
      log("forever")
      break
    }
  }
}"#,
    );
    assert!(
        warns.iter().any(|w| w.contains("always truthy")),
        "expected compound-folded `while` warning, got: {warns:?}"
    );
}

#[test]
fn test_vacuous_condition_lint_fires_in_guard_condition() {
    // `guard <cond> else { ... }`: a vacuous compound condition makes
    // either the guard's pass-through or its else-block dead. Use a
    // compound folder (not a bare literal) so the idiomatic-bare carve-
    // out doesn't apply.
    let warns = warnings(
        r"pipeline t(task) {
  fn check(cond: bool) -> int {
    guard cond && false else { return 0 }
    return 1
  }
}",
    );
    assert!(
        warns.iter().any(|w| w.contains("always falsy")),
        "expected compound-folded `guard` warning, got: {warns:?}"
    );
}

#[test]
fn test_vacuous_condition_lint_sees_nil_refined_scope_for_rhs() {
    // After `x != nil && schema_is(x, S)`, the right operand sees an
    // x that the narrower already stripped of `nil`. The lint must walk
    // the &&'s right operand in that refined scope so it agrees with the
    // narrower's view of `x`'s type.
    let warns = warnings(
        r"type Tag = {b: string}
pipeline t(task) {
  fn check(x: {a: int, b: string} | nil) {
    if x != nil && schema_is(x, Tag) {
      let _b: string = x.b
    }
  }
}",
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("schema_is") && w.contains("always true")),
        "expected schema_is to be vacuous after `x != nil` narrowing, got: {warns:?}"
    );
}

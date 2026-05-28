# HARN-LNT-058 — vacuous condition

## What it means

The condition of an `if`, `while`, or `guard` is statically known — one of
its two branches is unreachable. The lint fires for two patterns:

### 1. Constant-evaluable conditions

The condition reduces to a known boolean using only literal operands and
short-circuit / negation rules. Examples:

- `if true { … }`, `if false { … }` — direct booleans.
- `if nil { … }`, `if 0 { … }`, `if "" { … }` — falsy literals.
- `if (true || some_call()) { … }` — short-circuits to `true`.
- `if (some_call() && false) { … }` — short-circuits to `false`.
- `if !!true { … }` — chains of negations on a constant.

Side-effecting subexpressions inside a vacuous compound (`some_call()`
above) still execute, but their result is dead — usually a mistake left
behind by a partial refactor.

### 2. Statically-determined `schema_is` / `is_type`

The variable's static type already proves whether the predicate matches:

- **Always true** — `x`'s static type is a subtype of the schema, so the
  truthy branch narrows to nothing new and the falsy branch is dead.
  Example: `x: int` and `schema_is(x, int)`, or `x: {a: int, b: string}`
  and `schema_is(x, {b: string})` (width subtyping).
- **Always false** — `x`'s static type and the schema are disjoint. The
  truthy branch is dead. Example: `x: int` and `schema_is(x, string)`.

The check is deliberately conservative: it skips when `x: unknown` or
`x: any` (the open-world top types — `schema_is` is informative there),
and it requires shape fields to match optionality before reporting
"always true" (an optional field in `x` can be absent at runtime, so the
predicate may still legitimately fail). The same shape is used by
[`no-unnecessary-condition` in typescript-eslint][tse] and the
[`unnecessary-invariant` rule in Flow][flow].

[tse]: https://typescript-eslint.io/rules/no-unnecessary-condition/
[flow]: https://flow.org/en/docs/linting/rule-reference/#toc-unnecessary-invariant

## How to fix

- If the check is genuinely dead code, remove the surrounding `if` /
  `while` / `guard` and inline (or delete) the live branch.
- If you intended a *narrower* schema, change `S` to the narrower shape
  (e.g. a tagged variant, not the parent type).
- If the variable is meant to come from an untrusted boundary, type it
  as `unknown` (or `any`) and validate at the boundary; the lint will
  then correctly stay silent.

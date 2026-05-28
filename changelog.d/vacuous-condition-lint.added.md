- **New lint `HARN-LNT-058` (vacuous condition).** The typechecker now flags
  `if` / `while` / `guard` conditions whose result is statically determined,
  covering two patterns: (1) compound expressions that fold to a constant
  via short-circuit / negation rules — `if (false && cond)`,
  `if (true || cond)`, `if !!true`, etc. — using nil / bool / numeric /
  string literal leaves; and (2) `schema_is(x, S)` / `is_type(x, S)` whose
  answer is fixed by `x`'s static type. Bare `if true { … }` / `if false
  { … }` / `while true { … }` are intentionally skipped — they're the
  canonical Harn block-scope / disable-block / infinite-loop idioms, and
  the conformance suite (plus typical user code) relies on them. The
  schema case uses the same `intersect_types` / `types_compatible`
  machinery the narrower already uses, with a strict optional-vs-required
  check on shapes (a `{b: string?}` value can lack `b` at runtime, so it
  is *not* a guaranteed subtype of `{b: string}`). `unknown` and `any` are
  excluded — `schema_is` is genuinely informative on open-world top types.
  Modelled after Flow's `unnecessary-invariant` and typescript-eslint's
  `no-unnecessary-condition` (with `checkTypePredicates`).

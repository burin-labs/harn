# HARN-LNT-056 — ambient random builtin replaced by `harness.random.*`

**Category:** Lint (LNT)  
**Variant:** `Code::LintAmbientRandomBuiltin` (ambient random builtin)

## What it means

The lint fires on calls to the ambient `random`, `random_int`,
`random_choice`, and `random_shuffle` builtins. Randomness now routes
through the `harness.random.*` sub-handle so capability requirements
appear in the type system instead of being hidden in the stdlib surface.

This is a lint, not a hard error. The legacy builtins still compile
while the migration is in flight, but every new call site should use
the matching `harness.random.*` method (`random` →
`harness.random.gen_f64`, `random_int` → `harness.random.gen_range`,
etc.). Seeded streams via an explicit `Rng` handle remain available
through the `Rng.*` surface for tests that need deterministic output.

## How to fix

- Run `harn fix --apply --safety scope-local` over the file. The
  `bindings/thread-harness-random` repair rewrites every call site
  where a `harness` (or `_harness`) binding is in scope.
- If `harness` isn't reachable from the call site, first thread it
  through the enclosing fn via the `bindings/thread-harness` repair
  (which adds the `harness: Harness` parameter at the entrypoint), then
  re-run `harn fix --apply` to swap the call.

## Stability

This code is stable. Its identifier, category, and meaning will not
change without a deprecation cycle. Cross-language tooling and IDE
integrations can dispatch on it directly.

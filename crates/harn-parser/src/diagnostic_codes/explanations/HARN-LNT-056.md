# HARN-LNT-056 — ambient random builtin replaced by `harness.random.*`

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

- Run `harn fix --apply --safety scope-local` over the file. By default the
  fixer rewrites ambient random calls to the VM-level `harness` binding with
  `bindings/use-enclosing-harness-global`, preserving helper signatures.
- If you explicitly want source-level parameter threading, run
  `harn fix --apply --safety surface-changing --harness-threading thread-params`.
  `harn fix --plan --json` reports which signatures would change and whether
  cross-module callers must be updated.

# HARN-LNT-056 — ambient random builtin replaced by `harness.random.*`

## What it means

The lint fires on calls to the ambient `random`, `random_int`,
`random_choice`, and `random_shuffle` builtins. Randomness now routes
through the `harness.random.*` sub-handle so capability requirements
appear in the type system instead of being hidden in the stdlib surface.

The legacy effectful globals are removed. Use the matching
`harness.random.*` method (`random` → `harness.random.f64`, `random_int` →
`harness.random.range`, etc.). Seeded streams via an explicit `Rng` handle remain available
through the `Rng.*` surface for tests that need deterministic output.

## How to fix

- Run `harn fix --apply --safety surface-changing` over the file. Calls inside
  an existing Harness boundary are rewritten in place; otherwise the fixer
  threads an explicit Harness parameter through local callers.
- Run lint again. `capability-attenuation` suggests replacing an unnecessarily
  broad helper parameter with `HarnessRandom`.

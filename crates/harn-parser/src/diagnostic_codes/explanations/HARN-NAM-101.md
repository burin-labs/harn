# HARN-NAM-101 — `main` entrypoint must take a single `harness: Harness` parameter

## What it means

When a Harn program declares a top-level `fn main`, the runtime auto-invokes
it with the script's `Harness` capability handle. The convention is therefore
strict: the entrypoint must be exactly `fn main(harness: Harness) { ... }` —
or `fn main(_harness: Harness) { ... }` when the handle is intentionally
unused. It takes one parameter, named `harness` or `_harness`, typed
`Harness`, no defaults, no rest.

Any other shape (zero parameters, a renamed parameter, a missing or
non-`Harness` type annotation, extra parameters, or a default value) fails
this check before bytecode is emitted, so the runtime never tries to bind a
mismatched signature.

The `Harness` value gives the script typed access to its six capability
slices via field access (`harness.stdio`, `harness.clock`, `harness.fs`,
`harness.env`, `harness.random`, `harness.net`). Threading the handle through
`main` replaces every ambient stdio/clock/fs/env/random/net global.

## How to fix

Rewrite the entrypoint with the canonical signature:

```harn
fn main(harness: Harness) {
  harness.stdio.println("Hello, world!")
}
```

If you do not need any capabilities — for example, a script that only does
pure computation — prefix the parameter with `_` to mark it deliberately
unused:

```harn
fn main(_harness: Harness) {
  // pure logic …
}
```

If the function does not need to be the entrypoint, rename it (e.g. `helper`,
`run_once`) so the convention does not apply.

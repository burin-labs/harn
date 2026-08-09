# HARN-LNT-014 — unused variable lint

## How to fix

- Use the `_` discard binding for side-effect-only local expressions, for
  example `let _ = cleanup()`.
- Apply the lint's auto-fix where one is offered (`harn lint --fix`).
- Suppress the lint with an attribute only when the surrounding code is intentionally non-idiomatic.

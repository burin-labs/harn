# HARN-LNT-016 — unused parameter lint

## How to fix

- Apply the lint's auto-fix where one is offered (`harn lint --fix`).
- Functions, closures, public or extended pipelines, and fixture- or table-bound
  tests keep positional arity and prefix the unused parameter with `_`.
- Suppress the lint with an attribute only when the surrounding code is intentionally non-idiomatic.

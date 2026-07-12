# HARN-LNT-063 — unnecessary non-null assertion

A non-null assertion (`expr!`) was applied to a value whose type is already
non-nil, so the assertion does nothing and can be removed.

## How to fix

- Apply the lint's auto-fix where one is offered (`harn lint --fix`) to drop the
  trailing `!`.
- Suppress the lint with an attribute only when the surrounding code is
  intentionally non-idiomatic.

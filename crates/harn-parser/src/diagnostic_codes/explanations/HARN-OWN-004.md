# HARN-OWN-004 — unvalidated boundary value is used directly

**Category:** Ownership / mutability (OWN)  
**Variant:** `Code::BoundaryValueUnvalidated` (boundary value unvalidated)

## What it means

Harn's binding-and-mutability discipline rejects this usage. Bindings declared
`let` may not be reassigned; bindings declared `mut` should actually be
reassigned somewhere.

Specifically: unvalidated boundary value is used directly.

## How to fix

- Switch the binding kind (`let` ↔ `mut`) to match its actual use.
- Restructure so owned values do not escape their scope.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.

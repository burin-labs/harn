# HARN-OWN-001 — immutable binding is reassigned

**Category:** Ownership / mutability (OWN)  
**Variant:** `Code::ImmutableAssignment` (immutable assignment)

## What it means

Harn's binding-and-mutability discipline rejects this usage. Bindings declared
`let` may not be reassigned; bindings declared `mut` should actually be
reassigned somewhere.

Specifically: immutable binding is reassigned.

## How to fix

- Switch the binding kind (`let` ↔ `mut`) to match its actual use.
- Restructure so owned values do not escape their scope.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.

# HARN-TYP-006 — argument value does not match the parameter type

**Category:** Type checker (TYP)  
**Variant:** `Code::ArgumentTypeMismatch` (argument type mismatch)

## What it means

Harn's static type checker reports this when it cannot reconcile the types
involved at this position. Type errors block compilation — Harn refuses to run a
program whose types do not line up.

Specifically: argument value does not match the parameter type.

## How to fix

- Adjust the expression so its inferred type matches the surrounding context.
- Widen the declared type at the binding / parameter / return position to accept the actual type.
- Convert the value explicitly (`as`, a stdlib coercion, etc.) when a safe conversion exists.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.

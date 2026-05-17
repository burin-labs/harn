# HARN-TYP-018 — expression must be callable

**Category:** Type checker (TYP)  
**Variant:** `Code::CallableExpected` (callable expected)

## What it means

Harn's static type checker reports this when it cannot reconcile the types
involved at this position. Type errors block compilation — Harn refuses to run a
program whose types do not line up.

Specifically: expression must be callable.

## How to fix

- Adjust the expression so its inferred type matches the surrounding context.
- Widen the declared type at the binding / parameter / return position to accept the actual type.
- Convert the value explicitly (`as`, a stdlib coercion, etc.) when a safe conversion exists.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.

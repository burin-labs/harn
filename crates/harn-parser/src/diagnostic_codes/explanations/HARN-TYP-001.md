# HARN-TYP-001 — expected and actual types are incompatible

**Category:** Type checker (TYP)
**Variant:** `Code::TypeMismatch`

## What it means

Harn's type checker compared the inferred ("actual") type of an expression
against the type the surrounding context expects, and the two did not unify.
This is the most common type error — it covers any mismatch that doesn't fall
into one of the more specific TYP codes (assignment, argument, return, etc.).

## How to fix

- Adjust the expression so its inferred type matches the surrounding
  context.
- Widen the declared type at the binding / parameter / return position to
  accept the actual type.
- Convert the value explicitly (`as`, a stdlib coercion, etc.) when a safe
  conversion exists.
- If the mismatch is between a concrete type and an optional, use
  `?`-chaining or supply a default with `??`.

## See also

- HARN-TYP-004 — return-type mismatch.
- HARN-TYP-005 — assignment-type mismatch.
- HARN-TYP-006 — argument-type mismatch.
- HARN-TYP-007 — let-binding initializer type mismatch.
- HARN-TYP-009 — struct-field type mismatch.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.

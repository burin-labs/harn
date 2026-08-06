# HARN-TYP-001 — expected and actual types are incompatible

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

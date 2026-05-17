# HARN-TYP-014 — declaration has the wrong number of type parameters

**Category:** Type checker (TYP)
**Variant:** `Code::TypeParameterArity`

## What it means

A generic declaration (function, type alias, or struct) was written with a
different number of type parameters than the use-site supplies. Harn refuses
to silently fill in missing parameters or discard extra ones — the count must
match exactly.

## Example

```harn
fn pair<T>(a: T, b: T) -> [T; 2] { [a, b] }

// HARN-TYP-014: pair takes 1 type parameter, not 2
let xs = pair::<int, string>(1, "two")
```

## How to fix

- Supply exactly as many type arguments as the declaration declares, or omit
  the explicit list and let Harn infer them.
- If the declaration itself is wrong, edit the `<T, U, ...>` list on the
  declaration to match the arity the callers expect.
- For type aliases and structs, the same rule applies: `Map<K, V>` needs two
  arguments, not one and not three.

## See also

- HARN-TYP-013 — call site uses the wrong number of generic type arguments.
- HARN-TYP-015 — type argument violates a `where`-clause constraint.

## Stability

This code is stable. Its identifier, category, and meaning will not change
without a deprecation cycle. Cross-language tooling and IDE integrations can
dispatch on it directly.

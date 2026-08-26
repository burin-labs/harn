# HARN-TYP-029 — type predicate contract is invalid

A type predicate tells callers how a boolean result narrows one argument. Harn
checks the function body before trusting that claim.

This error means the declaration does not prove its contract. Common causes
include:

- The predicate names a missing, untyped, or rest parameter.
- The narrower type is not a subtype of the parameter type.
- The body does not end with one return condition.
- The true branch does not prove the narrower type.
- A two-sided predicate claims too much about the false branch.
- The predicate targets a generic type parameter.

## Fix it

Use a two-sided predicate when true and false both give exact type facts:

```harn
fn is_text(value: unknown) -> value is string {
  return type_of(value) == "string"
}
```

Add `implies` when only a true result proves the type:

```harn
fn is_nonempty_text(value: unknown) -> implies value is string {
  return type_of(value) == "string" && len(value) > 0
}
```

A false result in the second example may still mean an empty string, so Harn
does not narrow the false branch.

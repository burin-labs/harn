# Reuse narrowing checks

Use a `const` when a condition needs a clear name. Use a type predicate when
several functions need the same check.

## Name a condition

Bind the check with `const`, then branch on that name:

```harn
fn label(value: string | int) -> string {
  const kind = type_of(value)
  const is_text = kind == "string"
  if is_text {
    return value.upper()
  }
  return to_string(value + 1)
}
```

Use `const`, not `let`. A mutable value may change after the check.

## Share a two-sided check

Name the parameter and its narrower type after the return arrow:

```harn
fn is_text(value: unknown) -> value is string {
  return type_of(value) == "string"
}
```

Call the helper in an `if`, `guard`, or another condition:

```harn
fn normalize(value: string | int) -> string {
  if is_text(value) {
    return value.upper()
  }
  return to_string(value)
}
```

A false result also rules out `string` when the input is a closed union.

## Share a one-sided check

Add `implies` when false does not rule out the type:

```harn
fn is_nonempty_text(value: unknown) -> implies value is string {
  return type_of(value) == "string" && len(value) > 0
}
```

Here, false may mean an empty string. Harn narrows only the true branch.

Keep the helper body simple. It may contain `const` aliases followed by one
return condition. Harn reports `HARN-TYP-029` when the condition does not prove
the declared contract.

See [Type annotations](./spec/language/19-type-annotations.md#type-predicates)
for the full rules.

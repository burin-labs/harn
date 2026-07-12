# HARN-LNT-061 - nil coalesce fallback is nil

## What it means

`expr ?? nil` is equivalent to `expr`: when the left side is absent, the
nil-coalescing expression already evaluates to `nil`. The fallback does not
make the value safer or more explicit; it only adds a redundant branch that can
hide real defaulting logic.

This lint is warning-level because the code still behaves the same way.

## How to fix

Remove the `?? nil` fallback:

```harn
const value = task?.flag
```

Use a real fallback value when the expression needs a concrete default, such as
`false`, `0`, an empty list, or a typed record.

# HARN-LNT-061 - nil coalesce fallback is nil

## What it means

The fallback side of a nil-coalescing expression is the literal `nil`:

```harn,ignore
const value = task?.flag ?? nil
```

That expression is equivalent to `task?.flag`. If the left side is present, its
value is returned; if it is absent, `?? nil` returns `nil`, which is already the
left side's absent value.

## How to fix

Remove the fallback:

```harn
const value = task?.flag
```

Use a real default only when the surrounding code needs a non-nil value.

# HARN-LNT-061 - nil coalesce fallback has no effect

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

The same rule applies to `false` used as the exact positive condition of an
assertion:

```harn,ignore
assert(task?.ready ?? false)
```

`assert` accepts any value and applies Harn truthiness. Both `nil` and `false`
fail, so the fallback cannot change the result. Assert the value directly:

```harn
assert(task?.ready)
```

Keep boolean fallbacks used outside this exact position. In particular, a
negation or a `?? true` fallback can change how a missing value behaves.

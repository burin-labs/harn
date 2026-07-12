# HARN-LNT-065 - nil coalesce fallback repeats the left identifier

## What it means

Both sides of a nil-coalescing expression are the same identifier:

```harn,ignore
const value = task ?? task
```

For a plain identifier, this is equivalent to `task`. If `task` is present, the
left side wins; if it is `nil`, the fallback evaluates to the same `nil` value.

## How to fix

Remove the fallback:

```harn
const value = task
```

If a real recovery path is needed, replace the right side with an actual
default or explicitly handle `nil` before using the value.

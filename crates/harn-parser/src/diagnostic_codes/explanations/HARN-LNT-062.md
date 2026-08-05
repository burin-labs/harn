# HARN-LNT-062 - nil coalesce fallback is unreachable

## What it means

The left side of a `??` expression has a statically non-nil type, so the
fallback branch can never run. This often happens after a typed conversion or
producer already returns a concrete value:

```harn
fn parse_number(raw: string) -> int {
  return 0
}

const raw: string? = nil
const value = parse_number(raw ?? "0") ?? 0
```

The outer fallback is redundant when `parse_number(...)` has a non-nil result
type.

## How to fix

Remove the unreachable fallback:

```harn
fn parse_number(raw: string) -> int {
  return 0
}

const raw: string? = nil
const value = parse_number(raw ?? "0")
```

If the fallback really is needed, change the left expression or annotation so
its type accurately includes `nil`.

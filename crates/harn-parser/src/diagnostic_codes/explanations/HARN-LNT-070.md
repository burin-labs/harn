# HARN-LNT-070 — public positional API is ambiguous

A public function takes four or more positional parameters of the same type.
A caller who swaps two of them still type-checks, and nothing at the call site
says which value is which.

## How to fix

Take the group as one record instead, so the call site names each value:

```harn
pub fn draw_box(rect: {x: int, y: int, width: int, height: int}) {
  // ...
}

draw_box({x: 0, y: 0, width: 80, height: 24})
```

This is guidance about a public API's readability, not a limit on how many
parameters a function may have. It does not fire for private helpers, or for
signatures where the types already tell the values apart. Parameters with
defaults do count, since callers still pass them positionally. A rest parameter
does not.

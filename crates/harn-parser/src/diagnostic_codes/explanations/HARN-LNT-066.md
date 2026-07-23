# HARN-LNT-066 — the result of a pure collection method is discarded

A `list` / `dict` / `set` / `string` method was called as a statement and its
result thrown away, so the call has no effect at all.

Every method on Harn's built-in collections is **pure**. They are persistent,
copy-on-write values: `appending` clones the receiver, adds an item, and
returns a *new* list. It never modifies the receiver.

```harn
const l = []
l.appending(1)      // error[HARN-LNT-066]: no effect — `l` is still []
l.appending(2)      // error[HARN-LNT-066]
// l == []
```

The method name reads as a value-producing operation, but its result still has
to be consumed. Without this diagnostic, discarding it would typecheck, run
without error, and quietly build an empty list.

## How to fix

Assign the result back. Because the binding's value changes, it must be `let`
rather than `const` (see the binding rule in the language spec — `const` means
this binding's value never changes):

```harn
let l = []
l = l.appending(1)
l = l.appending(2)
// l == [1, 2]
```

When building a collection in a loop, the same shape applies:

```harn
let out = []
for item in items {
  out = out.appending(transform(item))
}
```

To call a method purely for a side effect inside it — which only a closure
argument can produce — the result is not discarded in the same sense, and this
lint does not fire. Prefer a `for` loop over `map` when you want effects.

## When it does not fire

- The tail expression of a value-producing block (a closure body, `match` arm,
  `if`/`else` branch, `try`, or `block { … }`), where the value *is* the
  block's result rather than a discarded statement.
- A call taking a closure argument (`items.map({ item -> … })`), which may
  perform effects and so is not provably inert.
- A receiver rooted at `harness`, whose methods exist for their effects.
- A method name the file also declares in an `impl` block, which may be a
  user-defined method with effects of its own.

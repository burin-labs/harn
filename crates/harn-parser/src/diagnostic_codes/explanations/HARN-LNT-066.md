# HARN-LNT-066 — the result of a pure collection method is discarded

A copy-returning `list` / `dict` / `set` / `string` method was called as a
statement and its result thrown away, so the call has no effect at all.

Copy-returning methods such as `sorted`, `reversed`, `lower`, and `windows`
leave the receiver alone and return a *new* value. Harn's collections are
persistent, copy-on-write values, so nothing else observes the call.

```harn
let l = [3, 1, 2]
l.sorted()      // error[HARN-LNT-066]: no effect — `l` is still [3, 1, 2]
// l == [3, 1, 2]
```

Without this diagnostic, discarding the result would typecheck, run without
error, and quietly leave the receiver unchanged.

## How to fix

Call the mutating counterpart to change the binding in place, which needs a
`let` binding (`const` means this binding's value never changes):

```harn
let l = [3, 1, 2]
l.sort()
// l == [1, 2, 3]
```

Or bind the new value:

```harn
const l = [3, 1, 2]
const ordered = l.sorted()
```

Mutating methods (`push`, `pop`, `insert`, `extend`, `clear`, `sort`,
`sort_by`, `reverse`, set `add`/`remove`, dict `remove`) change their receiver,
so calling one as a statement is the point and this lint never fires for them.

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

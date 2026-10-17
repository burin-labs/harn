## Scope rules

Harn uses lexical scoping with a parent-chain environment model.

### Environment

Each `HarnEnvironment` has:

- A `values` dictionary mapping names to `HarnValue`
- A `mutable` set tracking which names were declared with `let`
- An optional `parent` reference

### Variable lookup

`env.get(name)` checks the current scope's `values` first, then walks up the `parent` chain.
Returns `nil` (which becomes `.nilValue`) if not found anywhere.

### Variable definition

- `const name = value` -- defines `name` in the current scope. The
  binding's **value never changes**.
- `let name = value` -- defines `name` in the current scope. The
  binding's value **may** change.
- `let name = nil` -- leaves `name` widenable until the first non-`nil`
  assignment, which fixes the slot to `T | nil`. The explicit form
  `let name: T | nil = nil` remains valid when you want to pin `T`
  up front.
- `const _ = value` / `let _ = value` -- evaluate `value` and discard it
  without introducing a variable into scope. `_` can be reused any number
  of times in the same scope. This is also how you deliberately discard a
  result you do not need — see [Binding mutability](#binding-mutability).

Function parameters are immutable, exactly as if declared `const`.

### Binding mutability

Harn has one rule, and it covers every type:

> **`const` means this binding's value never changes. `let` means it may.**

Harn's collections are **value types** with copy-on-write representation:
assigning one to a second binding copies the value, so writes through the
copy are never observable through the original.

```harn
let original = {k: 1}
let copy = original
copy["k"] = 99
// original["k"] == 1 -- a copy, not an alias
```

That single fact settles what `const` forbids. Because a collection *is* a
value, changing part of it changes the binding's whole value:

- **Index, property, nested-path, and struct-field assignment** (`l[0] = x`,
  `d.k = x`, `d["a"]["b"] = x`, `p.x = 9`) all change the binding's value, so
  all of them require `let`. Through a `const` binding each is rejected
  identically, with `HARN-OWN-001`. Read `d["k"] = 1` as sugar for
  `d = <a new dict differing at k>` and the requirement is self-evident.
- **Mutating methods** (`xs.push(v)`, `xs.pop()`, `xs.sort()`, `s.add(v)`,
  `d.remove(k)`) are Swift's `mutating` methods: the call computes the
  receiver's next value and writes it back to the receiver's place. `xs.push(v)`
  is sugar for `xs = <xs with v appended>`, so the receiver must be a `let`
  binding or a property or subscript path rooted at one, and a `const`
  binding, a parameter, or a temporary is rejected with `HARN-OWN-001`. The
  call's own result is separate from the write: `pop()` returns the removed
  element.
- **Copy-returning methods** such as `sorted`, `reversed`, `lower`, and `merge`
  return a *new* value and leave the receiver's value untouched, so they
  change nothing and are permitted on a `const` binding. They are ordinary
  expressions, not assignments.

```harn
const base = [1]
// fine: base is untouched, base == [1]
const ordered = base.sorted()

let built = []
// this is how a collection is built up
built.push(1)
```

Because copy-returning methods are pure, discarding a result silently does
nothing; `HARN-LNT-066` rejects that as an error. A bare `l.sorted()` leaves
`l` unchanged and is always a bug; `l.sort()` is the in-place form.

#### Why this is Swift's rule, not TypeScript's

The rule follows from value semantics, not from the keyword's spelling.
TypeScript's `const` constrains only the binding, so `const a = []; a.push(1)`
is legal there — but TypeScript's arrays are *references*, and mutating one is
observable through every alias. Swift's `let` freezes an `Array` outright,
because a Swift `Array` is a *value*. Harn's collections are values, so Harn
takes Swift's position for the same reason.

The two halves are therefore one paradigm, not two: **values are immutable,
and a binding's value changes only through assignment.** Subscript assignment
is assignment, and a mutating method call is assignment to its receiver; an
alias taken before the call keeps the old value. Swift reaches the same place
from the other side, defining subscript-set and `mutating` methods on a value
type as whole-value reassignment.

### Variable assignment

`name = value` walks up the scope chain to find the binding. If the binding is found but was
declared with `const`, throws `HarnRuntimeError.immutableAssignment`. If not found in any scope,
throws `HarnRuntimeError.undefinedVariable`.

The same applies to every assignment form that changes a binding's value —
`l[0] = x`, `d.k = x`, `d["a"]["b"] = x`, `p.x = 9`, `l.push(x)` — since under
value semantics each one reassigns the binding as a whole. See
[Binding mutability](#binding-mutability).

### Scope creation

New child scopes are created for:

- Pipeline bodies
- `for` loop bodies (loop variable is mutable)
- `while` loop iterations
- `parallel`, `parallel each`, and `parallel settle` task bodies (isolated interpreter per task)
- `if`/`else` branch bodies and `match` arm bodies
- `try`/`catch` blocks (catch body gets its own child scope with optional error variable)
- Closure invocations (child of the *captured* environment, not the call site)
- `block` nodes

Control flow *headers* (`if` conditions, `match` scrutinees) evaluate in the current
scope, but each branch or arm body is its own child scope: bindings declared inside
shadow outer names and do not leak past the body.

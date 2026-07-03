## Interfaces

Interfaces define a set of method signatures that a struct type must
implement. Harn uses Go-style implicit satisfaction: a struct satisfies
an interface if its impl block contains all the required methods with
compatible signatures. There is no `implements` keyword. Interfaces may
also declare associated types.

### Interface declaration

```harn
interface Displayable {
  fn display(self) -> string
}

interface Serializable {
  fn serialize(self) -> string
  fn byte_size(self) -> int
}

interface Collection {
  type Item
  fn get(self, index: int) -> Item
}
```

Each method signature lists parameters (the first must be `self`) and an
optional return type. Associated types name implementation-defined types
that methods can refer to. The body is omitted -- interfaces only declare
the shape of the methods.

### Implicit satisfaction

A struct satisfies an interface when its `impl` block has all the methods
declared by the interface, with matching parameter counts:

```harn
struct Dog {
  name: string
}

impl Dog {
  fn display(self) -> string {
    return "Dog(${self.name})"
  }
}
```

`Dog` satisfies `Displayable` because it has a `display(self) -> string`
method. No extra annotation is needed.

### Using interfaces as type annotations

Interfaces can be used as parameter types. At compile time, the type
checker verifies that any struct passed to such a parameter satisfies
the interface:

```harn,ignore
fn show(item: Displayable) {
  log(item.display())
}

let d = Dog({name: "Rex"})
show(d)  // OK: Dog satisfies Displayable
```

### Generic constraints with interfaces

Interfaces can be used as generic constraints via `where` clauses:

```harn
fn process<T>(item: T) where T: Displayable {
  log(item.display())
}
```

The type checker verifies at call sites that the concrete type passed
for `T` satisfies `Displayable`. Passing a type that does not satisfy
the constraint produces a compile-time error. Generic parameters must bind
consistently across all arguments in the call, and container bindings such as
`list<T>` propagate the concrete element type instead of collapsing to an
unconstrained generic.

Bounds are full type expressions, so generic interfaces can be constrained
with their concrete arguments:

```harn,ignore
interface Sink<in T> { fn accept(self, value: T) -> nil }
fn drain<S>(sink: S) where S: Sink<int> { sink.accept(1) }
```

A type parameter may carry more than one bound, written either as repeated
clauses or additively -- the two forms are equivalent and `T` must satisfy
every bound:

```harn,ignore
fn describe<T>(item: T) -> string where T: Named, T: Aged { ... }
fn describe<T>(item: T) -> string where T: Named + Aged { ... }
```

Inside the body, a method call on `T` resolves against all of its bounds:
it is accepted when the method is declared on any bound interface, and a
call to a method on none of them is flagged.

### Subtyping and variance

Harn's subtype relation is *polarity-aware*: each compound type has a
declared variance per slot that determines whether widening (e.g.
`int <: float`) is allowed in that slot, prohibited entirely, or
applied with the direction reversed.

Type parameters on user-defined generics may be marked with `in` or
`out`:

```harn,ignore
type Reader<out T> = fn() -> T          // T appears only in output position
interface Sink<in T> { fn accept(v: T) -> int }
fn map<in A, out B>(value: A) -> B { ... }
```

| Marker | Meaning | Where T may appear |
|---|---|---|
| `out T` | covariant | output positions only |
| `in T` | contravariant | input positions only |
| (none) | invariant (default) | anywhere |

Unannotated parameters default to **invariant**. This is strictly
safer than implicit covariance — `Box<int>` does not flow into
`Box<float>` unless `Box` declares `out T` and the body uses `T`
only in covariant positions.

#### Built-in variance

| Constructor | Variance |
|---|---|
| `iter<T>` | covariant in `T` (read-only) |
| `list<T>` | invariant in `T` (mutable: `push`, index assignment) |
| `dict<K, V>` | invariant in both `K` and `V` (mutable) |
| `Result<T, E>` | covariant in both `T` and `E` |
| `fn(P1, ...) -> R` | parameters **contravariant**, return covariant |
| Shape `{ field: T, ... }` | covariant per field (width subtyping) |

The numeric widening `int <: float` only applies in covariant
positions. In invariant or contravariant positions it is suppressed —
that is what makes `list<int>` to `list<float>` a type error.

#### Function subtyping

For an actual `fn(A) -> R'` to be a subtype of an expected `fn(B) -> R`,
**`B` must be a subtype of `A`** (parameters are contravariant) and
`R'` must be a subtype of `R` (return is covariant). A callback that
accepts a wider input or produces a narrower output is always a valid
substitute.

```harn,ignore
let wide = fn(x: float) { return 0 }
let cb: fn(int) -> int = wide   // OK: float-accepting closure stands in for int-accepting

let narrow = fn(x: int) { return 0 }
let bad: fn(float) -> int = narrow   // ERROR: narrow cannot accept the float a caller may pass
```

#### Declaration-site checking

When a type parameter is marked `in` or `out`, the declaration body
is checked: each occurrence of the parameter must respect the
declared variance. Mismatches are caught at definition time, not at
each use:

```harn,ignore
type Box<out T> = fn(T) -> int
// ERROR: type parameter 'T' is declared 'out' (covariant) but appears
// in a contravariant position in type alias 'Box'
```

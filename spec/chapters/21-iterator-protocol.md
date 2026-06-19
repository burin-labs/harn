## Iterator protocol

Harn provides a lazy iterator protocol layered over the eager
collection methods. Eager methods (`list.map`, `list.filter`,
`list.flat_map`, `dict.map_values`, `dict.filter`, etc.) are
unchanged — they return eager collections. Lazy iteration is opt-in
via `.iter()` and the `iter(x)` builtin.

### The `Iter<T>` type

`Iter<T>` is a runtime value representing a lazy, single-pass, fused
iterator over values of type `T`. It is produced by calling `iter(x)`
or `x.iter()` on an iterable source (list, dict, set, string,
generator, channel) or by chaining a combinator on an existing iter.

`iter(x)` / `x.iter()` on a value that is already an `Iter<T>` is a
no-op (returns the iter unchanged).

### The `Pair<K, V>` type

`Pair<K, V>` is a two-element value used by the iterator protocol for
key/value and index/value yields.

- Construction: `pair(a, b)` builtin. Combinators such as `.zip` and
  `.enumerate` and dict iteration produce pairs automatically.
- Access: `.first` and `.second` as properties.
- For-loop destructuring: `for (k, v) in iter_expr { ... }` binds the
  `.first` and `.second` of each `Pair` to `k` and `v`.
- Equality: structural (`pair(1, 2) == pair(1, 2)`).
- Printing: `(a, b)`.

### For-loop integration

`for x in iter_expr` pulls values one at a time from `iter_expr` until
the iter is exhausted.

`for (a, b) in iter_expr` destructures each yielded `Pair` into two
bindings. If a yielded value is not a `Pair`, a runtime error is
raised.

`for entry in some_dict` (no `.iter()`) continues to yield
`{key, value}` dicts in sorted-key order for back-compat. Only
`some_dict.iter()` yields `Pair(key, value)`.

### Semantics

- **Lazy**: combinators allocate a new `Iter` and perform no work;
  values are only produced when a sink (or for-loop) pulls them.
- **Single-pass**: once an item has been yielded, it cannot be
  re-read from the same iter.
- **Fused**: once exhausted, subsequent pulls continue to report
  exhaustion (never panic, never yield again). Re-call `.iter()` on
  the source collection to obtain a fresh iter.
- **Snapshot**: lifting a list/dict/set/string `Rc`-clones the
  backing storage into the iter, so mutating the source after
  `.iter()` does not affect iteration.
- **String iteration**: yields chars (Unicode scalar values), not
  graphemes.
- **Printing**: `log(it)` / `to_string(it)` renders `<iter>` or
  `<iter (exhausted)>` without draining the iter.

### Combinators

Each combinator below is a method on `Iter<T>` and returns a new
`Iter` without consuming items eagerly.

| Method | Signature |
|---|---|
| `.iter()` | `Iter<T> -> Iter<T>` (no-op) |
| `.map(f)` | `Iter<T>, (T) -> U -> Iter<U>` |
| `.filter(p)` | `Iter<T>, (T) -> bool -> Iter<T>` |
| `.flat_map(f)` | `Iter<T>, (T) -> Iter<U> \| list<U> -> Iter<U>` |
| `.take(n)` | `Iter<T>, int -> Iter<T>` |
| `.skip(n)` | `Iter<T>, int -> Iter<T>` |
| `.take_while(p)` | `Iter<T>, (T) -> bool -> Iter<T>` |
| `.skip_while(p)` | `Iter<T>, (T) -> bool -> Iter<T>` |
| `.zip(other)` | `Iter<T>, Iter<U> -> Iter<Pair<T, U>>` |
| `.enumerate()` | `Iter<T> -> Iter<Pair<int, T>>` |
| `.chain(other)` | `Iter<T>, Iter<T> -> Iter<T>` |
| `.chunks(n)` | `Iter<T>, int -> Iter<list<T>>` |
| `.windows(n)` | `Iter<T>, int -> Iter<list<T>>` |

### Sinks

Sinks drive the iter to completion (or until a short-circuit) and
return an eager value.

| Method | Signature |
|---|---|
| `.to_list()` | `Iter<T> -> list<T>` |
| `.to_set()` | `Iter<T> -> set<T>` |
| `.to_dict()` | `Iter<Pair<K, V>> -> dict<K, V>` |
| `.count()` | `Iter<T> -> int` |
| `.sum()` | `Iter<T> -> int \| float` |
| `.min()` | `Iter<T> -> T \| nil` |
| `.max()` | `Iter<T> -> T \| nil` |
| `.reduce(init, f)` | `Iter<T>, U, (U, T) -> U -> U` |
| `.first()` | `Iter<T> -> T \| nil` |
| `.last()` | `Iter<T> -> T \| nil` |
| `.any(p)` | `Iter<T>, (T) -> bool -> bool` |
| `.all(p)` | `Iter<T>, (T) -> bool -> bool` |
| `.find(p)` | `Iter<T>, (T) -> bool -> T \| nil` |
| `.for_each(f)` | `Iter<T>, (T) -> any -> nil` |

### Notes

- `.to_dict()` requires the iter to yield `Pair` values; a runtime
  error is raised otherwise.
- `.min()` / `.max()` return `nil` on an empty iter.
- `.any` / `.all` / `.find` short-circuit as soon as the result is
  determined.
- Numeric ranges (`a to b`, `range(n)`) participate in the lazy iter
  protocol directly; applying any combinator on a `Range` returns a
  lazy `Iter` without materializing the range.


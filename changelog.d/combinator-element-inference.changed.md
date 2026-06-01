- **Iterator/collection combinators now infer their element type, and `map`/`flat_map`
  thread the closure's return type.** Previously `xs.map(...)`, `xs.filter(...)`,
  `xs.sort()` and `dict.keys()`/`values()`/`entries()` collapsed to the opaque
  `list`/`dict` type — only the lazy `Iter` combinators carried `T`, so a single
  eager combinator erased element typing for the rest of a chain. Now eager
  combinators preserve or transform the element type (`list<T>.filter(…) →
  list<T>`, `list<T>.map(f) → list<R>` where `R` is `f`'s inferred return,
  `dict<K,V>.entries() → list<Pair<K,V>>`, …), matching what the equivalent
  `.iter()`-bridged chain already produced. `map`/`flat_map` infer the closure
  body's return type with the closure parameter bound to the receiver's element
  type, so `[1,2,3].map({ n -> "v${n}" })` is now `list<string>`. Applies to both
  eager (`list`/`dict`) and lazy (`iter`) receivers.

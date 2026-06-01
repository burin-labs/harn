- **Destructured rest patterns now preserve the source's element/value type.**
  `let [head, ...rest] = xs` types `rest` as `list<T>` (was the opaque `list`),
  and `let { a, ...rest } = d` keeps `dict<K, V>` when the source dict is
  parameterized. Completes the destructuring type inference so iterating or
  indexing a rest binding recovers a precise element type.

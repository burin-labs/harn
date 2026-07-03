- **Generic inference joins conflicting candidates to a union.** `keep(1, "x")`
  against `fn keep<T>(a: T, b: T) -> T` now infers `T = int | string` (matching
  heterogeneous list-literal inference and TypeScript) instead of hard-erroring
  "type parameter 'T' was inferred as both int and string". Explicit type
  arguments (`identity<int>("oops")`) remain a frozen contract checked
  per-argument — they no longer run arg-driven re-inference at all.
- **`for`-`in` over a nilable iterable is now diagnosed.** Iterating a
  `list<T>?` (or statically-`nil`) value previously stripped the nil arm
  silently and threw at runtime; it now gets the same nilable-receiver error
  as property/subscript/method access.
- **`match` on a `bool` scrutinee must be exhaustive.** `match b { true -> … }`
  with no `false`/wildcard arm now errors like enum and union matches do.

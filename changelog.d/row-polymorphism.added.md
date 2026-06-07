- **Row polymorphism: open record types and row-polymorphic generics.** Shape
  types may now carry a trailing **row tail** — `{id: string, ...R}` is an open
  record (the listed fields plus a row variable `R` standing for any other
  fields), and `{...R1, ...R2}` is the right-biased merge of two rows. A
  function generic over rows types record merge precisely and soundly:

  ```harn
  pub fn merge<R1, R2>(a: {...R1}, b: {...R2}) -> {...R1, ...R2}
  ```

  `merge({a: 1}, {b: "x"})` now returns `{a: int, b: string}` — every field
  preserved with its real type, `b` overriding `a` on overlap — instead of
  failing to unify a single value type or collapsing to `dict`. Open-record
  parameters (`fn f(x: {id: string, ...rest})`) accept any record that has the
  required fields and carry the rest through. Row variables bind one-sidedly
  from the actual record's leftover fields; gradual tails (`dict`, `any`)
  interoperate, and absence reasoning stays restricted to closed shapes. `std`'s
  `merge` and `deep_merge` are re-typed with row signatures.

- **Record merge and spread now infer the precise merged shape.** `{...a, k: v}`,
  `{...a, ...b}`, and `a + b` on record shapes now produce the right-biased
  merged shape — every field carried through with its real type, later fields
  overriding earlier ones — instead of collapsing to an untyped `dict`. On an
  overlap the result is required if either side is required, and its type is the
  overriding (right) field's type, or the union of both when the right field is
  optional. Spreading a non-closed source (a `dict`, `dict<K,V>`, union, or
  unknown) still degrades to `dict` rather than inventing fields. This is the
  structural foundation for full row-polymorphism support. Generic functions
  also now bind a type parameter from a **named-alias** argument the same way
  they already did from an inline shape literal (`type Opts = {…}` arguments to
  `dict<string, V>` parameters no longer fail to infer `V`).

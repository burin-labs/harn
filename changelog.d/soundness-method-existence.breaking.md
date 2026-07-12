**`harn check` now rejects calls to methods that do not exist on a concrete
receiver (HARN-NAM-005).** Calling an unknown method on a value whose static
type is a `string`, `list`, `set`, `int`, `float`, `bool`, or a `struct` — for
example `(3.14).frobnicate()` or `user.age.frobnicate()` — is now a check
error with a "did you mean" suggestion, closing the soundness hole where such
calls passed `harn check` and then crashed (`has no method`) or silently
returned `nil` at runtime. Fields were already checked (HARN-NAM-004); methods
now are too. Gradual receivers (`unknown`/`any`, unconstrained generics,
`dict`/shape values that may hold a callable field, iterators, harness
handles) still defer to runtime, so dynamic code is unaffected.

- **Annotate stdlib return types in `std/math` and `std/cache`.** `std/math` helpers and public
  functions with provably concrete returns now carry `int` / `float` / `bool` / `list` / record
  annotations (including a named `KMeansResult`), and `std/cache`'s `with_cache_envelope` returns a
  named `CacheEnvelope` shape (its cached `value` stays `unknown`). Genuinely polymorphic numeric
  pass-throughs are left unannotated. No behavior change; `harn check --strict-types` over the stdlib
  stays at its pre-existing error count.

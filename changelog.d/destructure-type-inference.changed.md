- **Destructuring binds now infer the same types as the `?.`/`??` form they
  desugar to.** A destructured binding such as `let { path = "", retries = 0 }
  = opts` previously left `path` and `retries` untyped; the type checker now
  infers each binding from `field + default` exactly as the equivalent
  `opts?.path ?? ""` / `opts?.retries ?? 0` would — present shape fields keep
  their declared type, a `nil` default stays optional (`T | nil`), and the
  default's type carries through when the source dict is untyped. This makes
  migrating the pervasive `let x = input?.field ?? default` idiom to a single
  destructuring bind lossless under the type checker. Applies to `let`, `var`,
  and `for`-`in` patterns. See the new "Destructure with defaults" cookbook.
  (Positional/tuple-precise element types for list patterns remain a
  follow-up.)

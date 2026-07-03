- **Generic enum payloads bind their instantiated types in match patterns.**
  Matching a `Result<int, string>` with `Result.Ok(v)` used to bind `v` as the
  raw declaration parameter `T` (so `return v` from a typed fn errored with
  "expected int, found T"); the scrutinee's type arguments are now substituted,
  and statically-unknown instantiations degrade to gradual instead of leaking
  phantom parameter names.
- **Container writes are type-checked.** `xs[0] = v`, `d["k"] = v`, and
  `s.field = v` now validate the value against the element/value/field type,
  check subscript index types (`list` → `int`, `dict<K, V>` → `K`), and emit
  the same receiver diagnostics as reads (nilable receiver, unknown field on
  annotated shapes/structs). The unannotated dict-literal idiom stays lenient.
- **Flow narrowing is scope-chain aware.** Assigning inside a nested block or
  loop no longer produces spurious "assignment to `x`: expected string, found
  nil" errors on `string?` vars (the check target is the declared type), loop
  bodies invalidate narrowing for variables they reassign (both inside the loop
  and after it — `while` conditions re-narrow soundly), and path-narrowing
  invalidation now masks ancestor-scope entries instead of only local ones.
- **`type_of` narrowing recognises the full runtime tag vocabulary.**
  `type_of(x) == "duration"` (and `set`, `decimal`, `channel`, `range`, `pair`,
  …) now narrows like `list`/`dict` always did; the canonical tag list lives in
  `harn-builtin-meta` and a VM unit test keeps `VmValue::type_name` in lockstep.
- The HARN-OWN-001 immutable-assignment repair hints now say `var`/`let`
  instead of the nonexistent `mut` keyword.

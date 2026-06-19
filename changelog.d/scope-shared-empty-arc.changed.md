- **Entering a lexical block no longer allocates an empty binding map.** Every
  block pushes a scope, but inside a function body its bindings compile to local
  slots rather than env writes, so the pushed scope is almost always empty — yet
  it used to `Arc::new(BTreeMap::new())`-allocate (and free) one map per entry, a
  per-iteration cost in any loop whose body is a block. Empty scopes now share a
  single process-wide immutable map (a refcount bump), and the first real
  binding copies-on-write away from it, so scopes that never bind anything never
  allocate. No behavior change.

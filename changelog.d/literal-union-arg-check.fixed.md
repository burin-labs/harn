- **`harn check` now rejects an out-of-set literal passed to a string/int
  literal-union parameter.** Passing e.g. `"prepare"` where a parameter is
  typed `"submit" | "status" | "cancel" | "download"` was accepted statically
  and only failed at runtime with a `TypeError`. The checker now reproduces the
  VM's decision at check time — firing exactly when the value is a compile-time
  literal and the parameter is a homogeneous literal union (the shape the VM
  lowers to an `enum` schema), so runtime-valued data keeps its gradual-typing
  concession. This surfaced (and this release fixes) a latent bug where
  `harn models batch prepare` crashed because the stdlib passed `"prepare"` to
  a phase parameter whose type omitted it.

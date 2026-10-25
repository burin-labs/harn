`harn-hostlib` and `harn-package` no longer depend on `harn-runtime`. They
register builtins through `harn-vm`'s registry, so an embedder that wants every
builtin family must link `harn-runtime` itself, as the `harn` binary does. Both
crates now start compiling before the builtin families finish, which shortens
the rebuild after a `harn-vm` change. The JUnit parser moves back from
`harn-builtins` to `harn-vm`, because `harn-hostlib` calls it directly.

Migration: replace `harn_builtins::junit::` paths with
`harn_vm::stdlib::junit::`, and add a `harn-runtime` dependency to any embedder
that linked only `harn-hostlib` and expects every builtin family.

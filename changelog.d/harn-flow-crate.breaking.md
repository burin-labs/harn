- **Harn Flow moved out of `harn-vm` into the `harn-flow` crate.** Atoms,
  intents, slices, the Flow store, invariant predicates, and the `flow_*`
  builtins now compile in their own crate beside the VM, which shortens the
  serial `harn-vm` build. The new `harn-runtime` crate assembles the VM with
  every builtin family crate.

  Migration: replace `harn_vm::flow::` paths with `harn_flow::`. A binary
  that runs Harn code calls `harn_runtime::force_link()` instead of
  `harn_vm::stdlib::force_link()`, so the linker keeps every family's
  builtins.

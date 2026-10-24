- **Harn Flow moved out of `harn-vm` into the `harn-flow` crate.** Atoms,
  intents, slices, the Flow store, invariant predicates, and the `flow_*`
  builtins now compile in their own crate beside the VM, which shortens the
  serial `harn-vm` build. The new `harn-runtime` crate assembles the VM with
  every builtin family crate.

  Migration: replace `harn_vm::flow::` paths with `harn_flow::`. An embedder
  that runs Harn code registers the standard library through
  `harn_runtime::register_vm_stdlib` or `harn_runtime::register_core_stdlib`
  instead of the `harn_vm` functions of the same name, so the linker keeps
  every family's builtins.

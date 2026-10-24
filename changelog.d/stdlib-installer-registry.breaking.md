- **Builtin families register their own installers.** Each standard-library
  family declares its installer beside its `register_*_builtins` function
  with `harn_vm_core::stdlib_installer!`, and `register_vm_stdlib` runs every
  linked installer. A family's owning crate can therefore sit outside
  `harn-vm`. Installers may not register a name another installer registers,
  which a test now enforces, so installation order no longer matters.
  Families that keep thread-local state declare their reset the same way,
  and `reset_stdlib_state` runs every linked reset.

  Migration: replace `harn_vm::register_io_stdlib` and
  `harn_vm::register_agent_stdlib` with `harn_vm::register_vm_stdlib`, which
  installs the complete library. `harn_vm::register_core_stdlib` still
  installs only the deterministic builtins.

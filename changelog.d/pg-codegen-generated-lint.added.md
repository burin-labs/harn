- `harn pg codegen` now formats its rendered output, so generated type files
  are `harn fmt`-clean by construction.
- `harn lint` and `harn check` skip style and unused-declaration lints for
  machine-generated `*.generated.harn` files (type diagnostics still apply, and
  `harn fmt` still formats them). The signal is the filename, not an in-file
  `@generated`/`DO NOT EDIT` comment, so a generated marker cannot be pasted in
  to silence lints on a hand-written file.

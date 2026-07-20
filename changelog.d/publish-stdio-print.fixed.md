- Use `harness.stdio.print`/`eprint` in the crates.io publisher so recovery
  against older tagged Harn binaries typechecks (there is no `stdio.write`).

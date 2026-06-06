- **Experimental `harn-codegen` crate: a Cranelift-backed native compiler for
  Harn's scalar-compute subset.** Lowers `int`/`float`/`bool` functions
  (arithmetic, comparisons, logical ops, branches, loops, locals) from VM
  bytecode to native machine code, with an in-process JIT (`NativeFunction`),
  an object-file backend (`emit_object`), a pure-Rust reference interpreter,
  and a `harn-nativec` CLI. It is `publish = false` and is not a dependency of
  `harn-cli`/`harn-vm`, so the distributed binary never links Cranelift; build
  it explicitly with `-p harn-codegen`. See
  `docs/src/dev/native-codegen.md`.

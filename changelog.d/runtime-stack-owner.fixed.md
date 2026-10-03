- Every thread Harn creates in a crate that can parse or run Harn code now gets
  the runtime stack (32 MiB) from one owner, `harn_parser::runtime_stack`
  (re-exported as `harn_vm::runtime_stack`), instead of Rust's 2 MiB default.
  The check driver's parallel parse workers, the stdlib warm, I/O helpers, and
  test threads previously relied on `RUST_MIN_STACK`, which CI sets and shipped
  binaries do not. A workspace scan now refuses any other way to create a
  thread in those crates.

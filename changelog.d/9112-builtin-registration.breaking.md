Rust crates declaring builtins with `#[harn_builtin]` or
`harn_capability_method!` must depend directly on `harn-vm-core`. Generated
registrations now use the Core registry instead of a native stdlib module path.

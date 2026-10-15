Generated builtin registrations now use the Core registry instead of a native
stdlib module path. Core also owns their distributed registration dependency.

Migration: Rust crates declaring builtins with `#[harn_builtin]` or
`harn_capability_method!` must add a direct `harn-vm-core` dependency matching
their Harn version. Existing native type imports remain available through
reexports; new code can import the owning `harn-llm` and `harn-orchestration`
contracts directly.

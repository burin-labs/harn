The interpreter carries opaque runtime-owned Harness and MCP connection state.
Existing `Harness`, MCP connection constructors, and script behavior retain
their interfaces. `HarnessKind` is defined by
`harn-builtin-meta` and re-exported through its existing Harn VM paths.

Migration: Rust embedders must replace `handle.inner()` with
`handle.host_state::<harn_vm::harness::HarnessInner>()` and handle `None` when
the handle belongs to a different runtime backend.

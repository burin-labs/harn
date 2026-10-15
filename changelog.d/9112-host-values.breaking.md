The interpreter carries opaque runtime-owned Harness and MCP connection state.
Rust embedders that used `VmHarness::inner()` can recover native state with
`host_state::<harn_vm::harness::HarnessInner>()`; this returns `None` for a
different runtime backend. Existing `Harness`, MCP connection constructors,
and script behavior retain their interfaces. `HarnessKind` is defined by
`harn-builtin-meta` and re-exported through its existing Harn VM paths.

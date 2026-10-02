`harn_vm::tool_registry::ToolApplicationError` has a new public field,
`outcome: Option<ToolApplicationOutcome>`, set to `Error` or `Rejected` when a
handler declared the failure through the typed result envelope and `None` for
a declared throw. Its serialized form adds `"outcome"` only when it is set.

Migration: add the field to Rust struct literals.

```rust
ToolApplicationError { tool, data }                 // before
ToolApplicationError { tool, data, outcome: None }  // after
```

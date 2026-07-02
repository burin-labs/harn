Internal cleanup: collapsed the per-module copy-paste hostlib builtin
registration helpers into shared `BuiltinRegistry::register_fn` /
`register_gated_fn`, migrated hand-rolled stdlib argument helpers
(`bytes`, `files`, `multipart`, `observability`, `timing`) onto the
canonical `stdlib/options.rs` layer, and deleted dead test-only Rust
twins of the self-hosted `trace import` / `explain` CLI handlers
(their coverage now exercises the shipping `.harn` scripts
end-to-end). No user-facing behavior or error-message changes.

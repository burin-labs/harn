Provider configuration types now have their defining Rust crate in `harn-provider-catalog`.
Existing `harn_vm::llm_config` import paths remain reexports.

Migration: Keep existing imports, or import provider configuration directly from
`harn_provider_catalog::config`. Regenerate bindings or persisted type identities
that record the defining crate against `harn-provider-catalog`.

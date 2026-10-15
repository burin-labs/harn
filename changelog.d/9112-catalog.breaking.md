Provider configuration types now have their defining Rust crate in `harn-provider-catalog`.
Existing `harn_vm::llm_config` import paths remain reexports. Consumers that derive bindings or persisted
type identities from defining-crate paths must regenerate them against the lower crate.

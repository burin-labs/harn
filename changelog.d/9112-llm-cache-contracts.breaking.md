Provider usage normalization and prompt-cache conformance types are now
defined in `harn-llm`. The existing `harn_vm::llm::cache_conformance` exports
and `harn_vm::llm::usage::ReportedCacheUsage` remain available, and serialized
receipts retain their existing shapes. Runtime adapters still resolve active
provider configuration and measurements before classifying saved runs.

Migration: Existing imports need no changes. Code that records Rust
`std::any::type_name()` output for these types must account for their new
`harn_llm` defining paths. New consumers can depend on the lower crate directly.

The public API check reports this relocation as missing items because Rustdoc
JSON omits foreign re-exports, a known
[cargo-semver-checks limitation](https://github.com/obi1kenobi/cargo-semver-checks/issues/355).
The existing native paths are retained and checked by a compiled consumer.

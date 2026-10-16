Workflow verification contracts now normalize in `harn-orchestration`, with file resolution owned by the native runtime.
File, inline, and stage-field precedence, exact requirements, serialized shapes, and existing imports are preserved.

Migration: existing `harn_vm::orchestration` imports need no changes. Consumers that inspect Rust defining-crate paths
with `std::any::type_name` must accept `harn_orchestration::verification` for `VerificationContract` and
`VerificationRequirement`.

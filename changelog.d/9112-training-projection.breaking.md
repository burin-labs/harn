Training-example projection and response-event tool-call resolution now live in the
orchestration and LLM crates. Existing Native Rust import paths and serialized records
remain compatible; Rust `type_name` and defining-crate paths change for the re-exported types.

Migration: Existing imports need no changes. Consumers that persist Rust
`std::any::type_name()` values must accept `harn_orchestration::training_example`
for training-example types and `harn_llm::response_tool_calls` for `ResolveError`.
New consumers can use those lower-crate modules directly.

- **Rust-to-Harn stdlib crossings reuse immutable module instances (#9783).**
  A stdlib module whose top-level bindings are all immutable, and whose
  imports are too, is now instantiated once per VM execution tree and shared
  by every child VM, instead of being rebuilt and discarded on each crossing
  the parent had not imported. The text-tool parse that runs on every
  tool-bearing LLM response drops from about 3,700 to about 1,000 allocations
  per call. Modules with mutable top-level state, such as `std/llm/handlers`,
  keep their per-VM instance, so no state carries between crossings that
  previously saw a fresh module.

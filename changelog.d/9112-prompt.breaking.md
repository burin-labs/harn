Prompt-template syntax and tooling records now originate in `harn-prompt-syntax`.
The existing `harn_vm::stdlib::template` paths remain re-exports, while Rust
defining-crate identity and generated documentation reflect the new owner.

Migration: Keep existing imports, or import syntax, lint, outline, and filter
metadata directly from `harn_prompt_syntax`. Regenerate bindings that record
the defining crate. Rendering and Harn prompt syntax are unchanged.

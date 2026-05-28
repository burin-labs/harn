- **Docs + Display + drift test follow-ups to the `#[harn_builtin]`
  cutover.** Three small polish wins on the registry shipped in PR #2575:
  - **Doc sweep.** AGENTS.md, CONTRIBUTING.md, and module-level docstrings
    in `crates/harn-vm/src/stdlib.rs`, `crates/harn-vm/src/stdlib/macros.rs`,
    and `crates/harn-builtin-macros/src/lib.rs` no longer claim the legacy
    `SyncBuiltin` / `BuiltinGroup` / `register_builtin_group` DSL still
    survives — it was deleted, and the docs now reflect that. The
    "Looking ahead" linkme section in CONTRIBUTING.md is replaced with
    a "Captured-state pattern" note that points readers at the
    `thread_local!`-backed examples in `crates/harn-vm/src/checkpoint.rs`
    and `crates/harn-vm/src/metadata.rs`.
  - **`Display` for `BuiltinSignature` and `Ty`.** Renders a parsed
    sig back into the `#[harn_builtin]` `sig = "..."` grammar — recovers
    the `T?` and `number` sugars (the sig parser desugars both into
    unions). Lets downstream tools (LSP hover, `harn explain`, error
    formatting) emit a canonical form regardless of how the macro author
    typed the original sig string.
  - **Round-trip drift test.** New
    `crates/harn-vm/tests/builtin_signature_text_drift.rs` walks
    `ALL_BUILTIN_DEFS`, renders each parsed `BuiltinSignature` through
    `Display`, canonicalizes both sides (whitespace squash + sugar
    normalization), and asserts no drift. Catches future parser tweaks
    that would silently change how `a | b | c` associates or how
    `...rest` is parsed.

  Larger follow-ups filed as separate issues for later evaluation:
  #2584 (collapse VM opcode dispatch tables via `#[harn_opcode]`),
  #2585 (collapse `HookEvent` enum/parse/render), #2586 (measure +
  decide whether the `deferred_builtin` registration path is dead
  weight post-linkme).

- **Git hooks no longer validate generated artifacts with a stale binary.**
  The hooks decided whether to rebuild by looking for `.rs`/`Cargo` changes, but
  crates compile non-Rust assets in via `include_str!` — capability tables,
  diagnostic explanations, stdlib sources. Editing one changed what the binary
  emits without matching that pattern, so drift checks compared generated output
  against the old inputs and passed. The rebuild decision now covers every path
  under `crates/`.

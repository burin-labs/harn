//! Thin binary shim for the Harn language server.
//!
//! All logic lives in the `harn_lsp` library so the single multi-call `harn`
//! binary can dispatch into it (see `harn-cli`). This binary stays buildable
//! for local development and as a standalone artifact.

fn main() {
    harn_lsp::run();
}

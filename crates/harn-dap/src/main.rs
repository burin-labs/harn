//! Thin binary shim for the Harn debug adapter.
//!
//! All logic lives in the `harn_dap` library so the single multi-call `harn`
//! binary can dispatch into it (see `harn-cli`). This binary stays buildable
//! for local development and as a standalone artifact.

fn main() {
    harn_dap::run();
}

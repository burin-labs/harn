//! Consolidated integration-test binary for `harn-mcp-rc-compat`.
//!
//! Each former `tests/<name>.rs` integration file is now a submodule here
//! (`tests/harn_mcp_rc_compat/<name>.rs`). Collapsing the crate's separate integration
//! binaries into one cuts total link time and shrinks the `cargo nextest`
//! archive. Cargo auto-discovers `tests/harn_mcp_rc_compat/main.rs` as a single integration
//! target named `harn_mcp_rc_compat`; files under `tests/harn_mcp_rc_compat/` are not built as separate
//! binaries. Fixtures resolve via `CARGO_MANIFEST_DIR`, unaffected by the move.
//!
//! `recursion_limit` is a crate-level-only attribute, so the raised limit
//! `mcp_host` used to declare per file is hoisted here to cover the binary.
#![recursion_limit = "256"]

mod artifacts;
mod client;
mod generic_server;
mod legacy_compat;
mod mcp_host;

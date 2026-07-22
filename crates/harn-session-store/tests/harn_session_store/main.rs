//! Consolidated integration-test binary for `harn-session-store`.
//!
//! Each former `tests/<name>.rs` integration file is now a submodule here
//! (`tests/harn_session_store/<name>.rs`). Collapsing the crate's separate integration
//! binaries into one cuts total link time and shrinks the `cargo nextest`
//! archive. Cargo auto-discovers `tests/harn_session_store/main.rs` as a single integration
//! target named `harn_session_store`; files under `tests/harn_session_store/` are not built as separate
//! binaries. Fixtures resolve via `CARGO_MANIFEST_DIR`, unaffected by the move.

mod sqlite_concurrency;
mod sqlite_initialization;
mod store;

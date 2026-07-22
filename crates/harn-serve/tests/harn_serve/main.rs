//! Consolidated integration-test binary for `harn-serve`.
//!
//! Each former `tests/<name>.rs` integration file is now a submodule here
//! (`tests/harn_serve/<name>.rs`). Collapsing the crate's separate integration
//! binaries into one cuts total link time and shrinks the `cargo nextest`
//! archive. Cargo auto-discovers `tests/harn_serve/main.rs` as a single integration
//! target named `harn_serve`; files under `tests/harn_serve/` are not built as separate
//! binaries. Fixtures resolve via `CARGO_MANIFEST_DIR`, unaffected by the move.

mod limits_loadgen;
mod site_auth;
mod site_hosting;
mod site_raw_bodies;
mod site_streaming;
mod site_websocket;
mod streaming_conformance;
mod transport_conformance;

//! Consolidated integration-test binary for `harn-rules`.
//!
//! Each former `tests/<name>.rs` integration file is now a submodule here
//! (`tests/harn_rules/<name>.rs`). Collapsing the crate's separate integration
//! binaries into one cuts total link time and shrinks the `cargo nextest`
//! archive. Cargo auto-discovers `tests/harn_rules/main.rs` as a single integration
//! target named `harn_rules`; files under `tests/harn_rules/` are not built as separate
//! binaries. Fixtures resolve via `CARGO_MANIFEST_DIR`, unaffected by the move.

mod atomic_roundtrip;
mod data_tables;
mod lifecycle;
mod relational_composite;
mod safety_idempotency;
mod seed_pack;
mod where_transform_fix;

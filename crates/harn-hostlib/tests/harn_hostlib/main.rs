//! Consolidated integration-test binary for `harn-hostlib`.
//!
//! Every former `tests/<name>.rs` integration file is now a submodule of this
//! single binary (`tests/harn_hostlib/<name>.rs`). Collapsing ~33 separate
//! integration binaries into one cuts total link time and shrinks the
//! `cargo nextest` archive — each extra binary otherwise pays a full link and
//! is stored separately in the archive.
//!
//! Conventions for files under `tests/harn_hostlib/`:
//! * Fixtures are addressed through `CARGO_MANIFEST_DIR` (e.g.
//!   `tests/fixtures/ast/...`), so they resolve identically regardless of the
//!   source file's location.
//! * Platform-gated files keep their own `#![cfg(...)]` inner attribute, so a
//!   module simply compiles to nothing on unsupported targets — no `cfg` on the
//!   `mod` line is needed here.
//! * `recursion_limit` is a crate-level-only attribute, so the raised limit
//!   that `code_librarian_recall` and `smoke_harn_script` used to declare per
//!   file is hoisted here to cover the whole binary.
#![recursion_limit = "256"]

mod ast_builtins;
mod ast_fixtures;
mod ast_function_body_imports;
mod ast_language_coverage;
mod code_index;
mod code_index_cypher_recall;
mod code_index_graph_surface;
mod code_index_live_state;
mod code_index_scenario;
mod code_librarian_recall;
mod embed;
mod fs_path_scope;
mod fs_snapshot;
mod fs_staging;
mod fs_watch;
mod parser_agreement_corpus;
mod process_artifact_retention;
mod process_tools;
mod process_tools_background_schema;
mod process_tools_capture_transport;
mod process_tools_e2e;
mod process_tools_wait_command;
mod process_tools_wait_output;
mod registration;
mod sandbox_npm_offline_install;
mod scanner_e2e;
mod secret_store;
mod secret_store_os_native;
mod smoke_harn_script;
mod tools_file_io;
mod tools_git;
mod tools_outline;
mod tools_search;

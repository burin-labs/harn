//! `harn-hostlib`: opt-in host builtins for code intelligence (tree-sitter,
//! repo scanning, deterministic indexing) and tool execution (search, file
//! I/O, git, process lifecycle, file watcher).
//!
//! This crate is the Rust home of two classes of optional host capabilities:
//!
//! 1. **Code intelligence** — `ast/`, `code_index/`, `scanner/`, `fs_watch/`.
//! 2. **Deterministic tools** — `tools/` (search, fs, git, process).
//!
//! These don't belong inside `harn-vm` — pulling tree-sitter grammars,
//! ripgrep, and `notify` into the VM would balloon the footprint of every
//! pipeline that doesn't index host code. Instead, this crate exposes a
//! single [`HostlibCapability`] trait. Embedders such as `harn-cli`'s ACP
//! server) compose the modules they need via [`HostlibRegistry`] and wire
//! the resulting builtins into the VM through [`harn_vm::Vm::register_builtin`]
//! / [`harn_vm::Vm::register_async_builtin`].
//!
//! ## Status
//!
//! The AST, scanner, code-index, and deterministic-tool surfaces are
//! implemented. `fs_watch/` still registers its public contract with
//! [`HostlibError::Unimplemented`] handlers. Module names, method names,
//! and JSON schemas under `schemas/` are the source of truth for hostlib
//! request/response compatibility, so they must stay stable while module
//! bodies evolve.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod ast;
pub mod code_index;
pub mod error;
pub mod fs_watch;
pub mod scanner;
pub mod schemas;
pub mod tools;

mod registry;

pub use error::HostlibError;
pub use registry::{BuiltinRegistry, HostlibCapability, HostlibRegistry, RegisteredBuiltin};

/// Convenience: build a `HostlibRegistry` populated with every capability
/// the crate ships, register them on the supplied VM, and return the
/// registry so callers can introspect (e.g. for schema-drift tests).
///
/// This is the canonical entry point for embedders that want the full
/// hostlib surface; pick-and-choose embedders should construct
/// [`HostlibRegistry`] directly.
pub fn install_default(vm: &mut harn_vm::Vm) -> HostlibRegistry {
    let mut registry = HostlibRegistry::new()
        .with(ast::AstCapability)
        .with(code_index::CodeIndexCapability::new())
        .with(scanner::ScannerCapability)
        .with(fs_watch::FsWatchCapability)
        .with(tools::ToolsCapability);
    registry.register_into_vm(vm);
    registry
}

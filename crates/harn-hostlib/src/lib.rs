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

#[cfg(feature = "ast")]
pub mod ast;
#[cfg(feature = "ast")]
pub mod code_index;
#[cfg(feature = "computer")]
pub mod computer;
pub mod embed;
pub mod error;
pub mod fs;
pub mod fs_snapshot;
pub mod fs_watch;
pub mod host_env_custody;
pub mod host_lease;
pub mod process;
mod process_liveness;
pub mod sandbox;
pub mod scanner;
pub mod schemas;
pub mod secret_store;
#[cfg(feature = "terminal-session")]
pub mod terminal_session;
pub mod tools;
pub mod verdict;

mod json;
mod registry;
mod text;
mod value_args;

pub use error::HostlibError;
pub use host_lease::{
    HostLeaseAcquireReceipt, HostLeaseAcquireStatus, HostLeaseCargoExecutionContext,
    HostLeaseDeferReason, HostLeaseDeferReceipt, HostLeaseError, HostLeaseExecutionContext,
    HostLeaseHandle, HostLeaseOperationKind, HostLeasePathIdentity, HostLeasePriorityClass,
    HostLeaseProcessExit, HostLeaseReleaseReceipt, HostLeaseRenewReceipt, HostLeaseRequest,
    HostLeaseResourceClass, HostLeaseResourceDefinition, HostLeaseResourceKey,
    HostLeaseRunLaunchFailure, HostLeaseRunReceipt, HostLeaseRunReleaseOutcome,
    HostLeaseRunStartFailure, HostLeaseRunState, HostLeaseState, HostLeaseStore,
    HOST_LEASE_ROOT_ENV,
};
pub use registry::{BuiltinRegistry, HostlibCapability, HostlibRegistry, RegisteredBuiltin};

/// Convenience: build a `HostlibRegistry` populated with every capability
/// the crate ships, register them on the supplied VM, and return the
/// registry so callers can introspect (e.g. for schema-drift tests).
///
/// This is the canonical entry point for embedders that want the full
/// hostlib surface; pick-and-choose embedders should construct
/// [`HostlibRegistry`] directly.
pub fn install_default(vm: &mut harn_vm::Vm) -> HostlibRegistry {
    let mut registry = HostlibRegistry::new();
    // The code-intelligence capabilities (`ast` + `code_index`) are only
    // compiled when the `ast` feature is on. Lean clients that omit it get
    // the deterministic tool surface without tree-sitter or any grammar.
    #[cfg(feature = "ast")]
    {
        let code_index = code_index::CodeIndexCapability::new();
        registry = registry
            .with(ast::AstCapabilityWithCodeIndex::new(code_index.shared()))
            .with(code_index);
    }
    registry = registry
        .with(scanner::ScannerCapability)
        .with(embed::EmbedCapability::default())
        .with(fs::FsCapability)
        .with(fs_snapshot::FsSnapshotCapability)
        .with(fs_watch::FsWatchCapability)
        .with(tools::ToolsCapability)
        .with(secret_store::SecretStoreCapability)
        .with(verdict::VerdictCapability);
    #[cfg(feature = "terminal-session")]
    {
        registry = registry.with(terminal_session::TerminalSessionCapability::new());
    }
    // Computer use (screenshot + mouse/keyboard) is opt-in at the feature
    // level AND default-deny at runtime: even with `computer-local` compiled,
    // the backend is a NullBackend unless `BURIN_COMPUTER_USE_TRANSPORT` is
    // explicitly set to `local` (or `helper`/`remote`). In the product it is
    // gated again by an off-by-default setting. Registering the builtins is
    // therefore harmless when unarmed — every call fails with an explanatory
    // message until the transport is explicitly chosen.
    #[cfg(feature = "computer")]
    {
        registry = registry.with(computer::ComputerUseCapability::new());
    }
    registry.register_into_vm(vm);
    registry
}

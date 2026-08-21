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
pub mod host_conditions;
pub mod host_env_custody;
pub mod host_lease;
pub mod host_lease_capability;
pub mod process;
mod process_liveness;
pub mod sandbox;
pub mod scanner;
pub mod schemas;
pub mod secret_store;
pub mod session;
#[cfg(feature = "terminal-session")]
pub mod terminal_session;
pub mod tools;
pub mod verdict;

mod json;
mod registry;
mod text;
mod value_args;

pub use error::HostlibError;
pub use host_conditions::{
    HostConditionObservation, HostConditionStatus, HostConditionsCapability,
    HostConditionsSnapshot, HostConditionsSource, HostContentionQuestion, HostEnvironment,
    InjectedHostConditionsSource, LocalHostConditionsSource, HOST_CONDITIONS_SCHEMA_VERSION,
};
pub use host_lease::{
    HostLeaseAcquireReceipt, HostLeaseAcquireStatus, HostLeaseCargoExecutionContext,
    HostLeaseDeferReason, HostLeaseDeferReceipt, HostLeaseError, HostLeaseExecutionContext,
    HostLeaseHandle, HostLeaseMetadataUpdateReceipt, HostLeaseOperationKind, HostLeasePathIdentity,
    HostLeasePriorityClass, HostLeaseProcessExit, HostLeaseQueueEvidence, HostLeaseReleaseReceipt,
    HostLeaseRenewReceipt, HostLeaseRequest, HostLeaseResourceClass, HostLeaseResourceDefinition,
    HostLeaseResourceKey, HostLeaseRunLaunchFailure, HostLeaseRunReceipt,
    HostLeaseRunReleaseOutcome, HostLeaseRunStartFailure, HostLeaseRunState, HostLeaseState,
    HostLeaseStore, DEFAULT_HOST_LEASE_DOMAIN, HOST_LEASE_ROOT_ENV,
};
pub use registry::{BuiltinRegistry, HostlibCapability, HostlibRegistry, RegisteredBuiltin};

/// Handles retained from [`install_default_with_handles`] so embedders can
/// warm or introspect capabilities out-of-band of the VM.
pub struct DefaultHostlibHandles {
    /// Shared code-index capability when the `ast` feature is enabled.
    #[cfg(feature = "ast")]
    pub code_index: code_index::CodeIndexCapability,
}

/// Convenience: build a `HostlibRegistry` populated with every capability
/// the crate ships, register them on the supplied VM, and return the
/// registry so callers can introspect (e.g. for schema-drift tests).
///
/// This is the canonical entry point for embedders that want the full
/// hostlib surface; pick-and-choose embedders should construct
/// [`HostlibRegistry`] directly. Embedders that need the retained
/// [`code_index::CodeIndexCapability`] handle (for
/// [`code_index::CodeIndexCapability::warm_session`]) should call
/// [`install_default_with_handles`] instead.
pub fn install_default(vm: &mut harn_vm::Vm) -> HostlibRegistry {
    install_default_with_handles(vm).0
}

/// Like [`install_default`], but also returns retained capability handles.
///
/// The code-index handle shares the same [`code_index::SharedIndex`] cell
/// installed into the VM, so a session-start
/// [`code_index::CodeIndexCapability::warm_session`] populates the index
/// visible to later agent turns.
pub fn install_default_with_handles(
    vm: &mut harn_vm::Vm,
) -> (HostlibRegistry, DefaultHostlibHandles) {
    let mut registry = HostlibRegistry::new();
    let embed = embed::EmbedCapability::default();
    let session = session::SessionCapability::with_embedder(embed.embedder().clone());
    // The code-intelligence capabilities (`ast` + `code_index`) are only
    // compiled when the `ast` feature is on. Lean clients that omit it get
    // the deterministic tool surface without tree-sitter or any grammar.
    #[cfg(feature = "ast")]
    let code_index_handle = {
        let code_index = code_index::CodeIndexCapability::new();
        let handle = code_index.clone();
        registry = registry
            .with(ast::AstCapabilityWithCodeIndex::new(code_index.shared()))
            .with(code_index);
        handle
    };
    registry = registry
        .with(scanner::ScannerCapability)
        .with(embed)
        .with(session)
        .with(fs::FsCapability)
        .with(fs_snapshot::FsSnapshotCapability)
        .with(fs_watch::FsWatchCapability)
        .with(tools::ToolsCapability)
        .with(secret_store::SecretStoreCapability)
        .with(verdict::VerdictCapability)
        .with(host_conditions::HostConditionsCapability::default())
        .with(host_lease_capability::HostLeaseCapability);
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
    // Compatibility stub: typed `HarnessTools` replaced the thread-local
    // `hostlib_enable` gate. Legacy ambient callers still invoke
    // `hostlib_enable("tools:deterministic")` before hostlib_* builtins; keep
    // that spelling as a no-op so dispatch does not fall through to an
    // embedder host bridge.
    vm.register_builtin("hostlib_enable", |_args, _out| Ok(harn_vm::VmValue::Nil));
    let handles = DefaultHostlibHandles {
        #[cfg(feature = "ast")]
        code_index: code_index_handle,
    };
    (registry, handles)
}

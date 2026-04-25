//! Code index host capability.
//!
//! Ports the deterministic trigram/word index that lived in
//! `Sources/BurinCodeIndex/` on the Swift side. Exposes five host builtins
//! whose surfaces are locked by `schemas/code_index/<method>.json`:
//!
//! | Builtin                          | What it does                                           |
//! |----------------------------------|--------------------------------------------------------|
//! | `hostlib_code_index_query`       | Trigram-accelerated literal substring search.          |
//! | `hostlib_code_index_rebuild`     | Walk a workspace and (re)build the in-memory index.    |
//! | `hostlib_code_index_stats`       | Count files/trigrams/words + last rebuild timestamp.   |
//! | `hostlib_code_index_imports_for` | Imports declared by a single file (with resolutions).  |
//! | `hostlib_code_index_importers_of`| Reverse lookup: who imports the given module/path?     |
//!
//! The capability owns one [`SharedIndex`] cell per instance — there is at
//! most one live workspace per VM at a time. The schema for the read
//! builtins (`query`, `stats`, `imports_for`, `importers_of`) does not
//! carry a workspace argument, so multi-workspace embedders are expected
//! to drive separate `CodeIndexCapability` handles. The internal layout
//! still keeps everything keyed on `FileId`, so a future schema bump can
//! add a `root` parameter without refactoring the data model.

mod builtins;
mod file_table;
mod graph;
mod imports;
mod state;
mod trigram;
mod walker;
mod words;

use std::sync::{Arc, Mutex};

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::registry::{BuiltinRegistry, HostlibCapability, RegisteredBuiltin, SyncHandler};

pub use builtins::SharedIndex;
pub use file_table::{FileId, IndexedFile, IndexedSymbol};
pub use graph::DepGraph;
pub use state::{BuildOutcome, IndexState};
pub use trigram::TrigramIndex;
pub use words::{WordHit, WordIndex};

/// Code-index capability handle.
///
/// Holds the [`SharedIndex`] cell behind an `Arc<Mutex<...>>`; cloning the
/// capability shares state. Embedders that want isolated workspaces should
/// construct independent instances.
#[derive(Clone, Default)]
pub struct CodeIndexCapability {
    index: SharedIndex,
}

impl CodeIndexCapability {
    /// Create a capability with an empty workspace slot. The first
    /// `hostlib_code_index_rebuild` call populates it.
    pub fn new() -> Self {
        Self {
            index: Arc::new(Mutex::new(None)),
        }
    }

    /// Borrow the underlying shared cell. Useful for tests and embedders
    /// that want to introspect index state without going through the
    /// builtins.
    pub fn shared(&self) -> SharedIndex {
        self.index.clone()
    }
}

impl HostlibCapability for CodeIndexCapability {
    fn module_name(&self) -> &'static str {
        "code_index"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_QUERY,
            "query",
            builtins::run_query,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_REBUILD,
            "rebuild",
            builtins::run_rebuild,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_STATS,
            "stats",
            builtins::run_stats,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_IMPORTS_FOR,
            "imports_for",
            builtins::run_imports_for,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_IMPORTERS_OF,
            "importers_of",
            builtins::run_importers_of,
        );
    }
}

fn register(
    registry: &mut BuiltinRegistry,
    index: SharedIndex,
    name: &'static str,
    method: &'static str,
    runner: fn(&SharedIndex, &[VmValue]) -> Result<VmValue, HostlibError>,
) {
    let captured = index;
    let handler: SyncHandler = Arc::new(move |args| runner(&captured, args));
    registry.register(RegisteredBuiltin {
        name,
        module: "code_index",
        method,
        handler,
    });
}

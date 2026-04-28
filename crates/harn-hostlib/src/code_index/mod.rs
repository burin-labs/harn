//! Code index host capability.
//!
//! Ports the deterministic trigram/word index plus the live workspace
//! state (agent registry, advisory locks, append-only version log, file
//! id assignment, cached reads) that previously lived in
//! `Sources/BurinCodeIndex/` on the Swift side. The capability owns one
//! [`SharedIndex`] cell per instance; cloning the capability shares
//! state with every Harn VM that has been wired against it.
//!
//! Surface — every builtin is locked by `schemas/code_index/<method>.json`:
//!
//! ### Workspace queries (the original 5)
//!
//! | Builtin                          | What it does                                           |
//! |----------------------------------|--------------------------------------------------------|
//! | `hostlib_code_index_query`       | Trigram-accelerated literal substring search.          |
//! | `hostlib_code_index_rebuild`     | Walk a workspace and (re)build the in-memory index.    |
//! | `hostlib_code_index_stats`       | Count files/trigrams/words + last rebuild timestamp.   |
//! | `hostlib_code_index_imports_for` | Imports declared by a single file (with resolutions).  |
//! | `hostlib_code_index_importers_of`| Reverse lookup: who imports the given module/path?     |
//!
//! ### Live workspace state (added in #776)
//!
//! - **Agents**: `agent_register`, `agent_heartbeat`, `agent_unregister`,
//!   `current_agent_id`, `status`.
//! - **Locks**: `lock_try`, `lock_release`.
//! - **Change log**: `current_seq`, `changes_since`, `version_record`.
//! - **File table**: `path_to_id`, `id_to_path`, `file_ids`, `file_meta`,
//!   `file_hash`.
//! - **Cached reads**: `read_range`, `reindex_file`, `trigram_query`,
//!   `extract_trigrams`, `word_get`, `deps_get`, `outline_get`.
//!
//! ## Concurrency model
//!
//! All ops serialise through a single `Arc<Mutex<Option<IndexState>>>`.
//! That matches the Swift actor: the IDE editor, eval, and live agent all
//! see one consistent view. The capability is `Send + Sync` so embedders
//! can share it across threads, but the mutex still serialises actual
//! work.

mod agents;
mod builtins;
mod file_table;
mod graph;
mod imports;
mod snapshot;
mod state;
mod trigram;
mod versions;
mod walker;
mod words;

use std::path::Path;
use std::sync::{Arc, Mutex};

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::registry::{BuiltinRegistry, HostlibCapability, RegisteredBuiltin, SyncHandler};

pub use agents::{AgentId, AgentInfo, AgentRegistry, AgentState, RegistryConfig};
pub use builtins::SharedIndex;
pub use file_table::{FileId, IndexedFile, IndexedSymbol};
pub use graph::DepGraph;
pub use snapshot::{CodeIndexSnapshot, SnapshotMeta};
pub use state::{BuildOutcome, IndexState};
pub use trigram::TrigramIndex;
pub use versions::{ChangeRecord, EditOp, VersionEntry, VersionLog, HISTORY_LIMIT};
pub use words::{WordHit, WordIndex};

/// Code-index capability handle.
///
/// Holds the [`SharedIndex`] cell behind an `Arc<Mutex<...>>`; cloning
/// the capability shares state. The capability also threads a
/// `current_agent_id` slot used by the `current_agent_id` host builtin —
/// embedders update this slot from the request-handling layer so each
/// host call surfaces the right agent identity to scripts.
#[derive(Clone, Default)]
pub struct CodeIndexCapability {
    index: SharedIndex,
    current_agent: Arc<Mutex<Option<AgentId>>>,
}

impl CodeIndexCapability {
    /// Create a capability with an empty workspace slot. The first
    /// `hostlib_code_index_rebuild` call populates it.
    pub fn new() -> Self {
        Self {
            index: Arc::new(Mutex::new(None)),
            current_agent: Arc::new(Mutex::new(None)),
        }
    }

    /// Borrow the underlying shared cell. Useful for tests and embedders
    /// that want to introspect index state without going through the
    /// builtins.
    pub fn shared(&self) -> SharedIndex {
        self.index.clone()
    }

    /// Borrow the current-agent slot. Embedders bind this slot before
    /// dispatching a host call so that `current_agent_id` returns the
    /// right value to the script.
    pub fn current_agent_slot(&self) -> Arc<Mutex<Option<AgentId>>> {
        self.current_agent.clone()
    }

    /// Convenience: set the current agent id. Returns the previous value
    /// (so callers can restore on completion if they bind per-call).
    pub fn set_current_agent(&self, id: Option<AgentId>) -> Option<AgentId> {
        let mut guard = self.current_agent.lock().expect("current_agent poisoned");
        std::mem::replace(&mut *guard, id)
    }

    /// Restore from a previously saved snapshot at
    /// `<root>/.burin/index/snapshot.json`. After restoring, runs
    /// [`IndexState::reap_after_recovery`] so stale agent records and
    /// locks are dropped before the daemon serves traffic.
    ///
    /// Returns `true` on a successful restore, `false` if no snapshot
    /// existed (or the format was unrecognised). Errors propagate I/O
    /// problems verbatim so callers can decide whether to fall back to
    /// `rebuild`.
    pub fn restore_from_disk(&self, workspace_root: &Path) -> std::io::Result<bool> {
        match CodeIndexSnapshot::load(workspace_root)? {
            Some(snap) => {
                let mut state = IndexState::from_snapshot(snap);
                state.reap_after_recovery(state::now_unix_ms());
                let mut guard = self.index.lock().expect("code_index mutex poisoned");
                *guard = Some(state);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Persist the current in-memory state to
    /// `<root>/.burin/index/snapshot.json`. Returns `Ok(false)` when the
    /// capability is empty (nothing to save).
    pub fn persist_to_disk(&self) -> std::io::Result<bool> {
        let snap = {
            let guard = self.index.lock().expect("code_index mutex poisoned");
            guard
                .as_ref()
                .map(|state| (state.snapshot(), state.root.clone()))
        };
        match snap {
            Some((snap, root)) => {
                snap.save(&root)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

impl HostlibCapability for CodeIndexCapability {
    fn module_name(&self) -> &'static str {
        "code_index"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        // Workspace queries (original 5).
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

        // File table accessors.
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_PATH_TO_ID,
            "path_to_id",
            builtins::run_path_to_id,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_ID_TO_PATH,
            "id_to_path",
            builtins::run_id_to_path,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_FILE_IDS,
            "file_ids",
            builtins::run_file_ids,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_FILE_META,
            "file_meta",
            builtins::run_file_meta,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_FILE_HASH,
            "file_hash",
            builtins::run_file_hash,
        );

        // Cached read paths.
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_READ_RANGE,
            "read_range",
            builtins::run_read_range,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_REINDEX_FILE,
            "reindex_file",
            builtins::run_reindex_file,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_TRIGRAM_QUERY,
            "trigram_query",
            builtins::run_trigram_query,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_EXTRACT_TRIGRAMS,
            "extract_trigrams",
            builtins::run_extract_trigrams,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_WORD_GET,
            "word_get",
            builtins::run_word_get,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_DEPS_GET,
            "deps_get",
            builtins::run_deps_get,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_OUTLINE_GET,
            "outline_get",
            builtins::run_outline_get,
        );

        // Change log.
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_CURRENT_SEQ,
            "current_seq",
            builtins::run_current_seq,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_CHANGES_SINCE,
            "changes_since",
            builtins::run_changes_since,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_VERSION_RECORD,
            "version_record",
            builtins::run_version_record,
        );

        // Agent registry + locks.
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_AGENT_REGISTER,
            "agent_register",
            builtins::run_agent_register,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_AGENT_HEARTBEAT,
            "agent_heartbeat",
            builtins::run_agent_heartbeat,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_AGENT_UNREGISTER,
            "agent_unregister",
            builtins::run_agent_unregister,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_LOCK_TRY,
            "lock_try",
            builtins::run_lock_try,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_LOCK_RELEASE,
            "lock_release",
            builtins::run_lock_release,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_STATUS,
            "status",
            builtins::run_status,
        );

        // `current_agent_id` is the only handler that reads from the
        // capability's per-call `current_agent` slot rather than the
        // index state, so it gets its own closure.
        let slot = self.current_agent.clone();
        let handler: SyncHandler =
            Arc::new(move |args| builtins::run_current_agent_id(&slot, args));
        registry.register(RegisteredBuiltin {
            name: builtins::BUILTIN_CURRENT_AGENT_ID,
            module: "code_index",
            method: "current_agent_id",
            handler,
        });
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

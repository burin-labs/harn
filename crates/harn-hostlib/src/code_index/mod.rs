//! Code index host capability.
//!
//! Deterministic trigram/word index plus live workspace state (agent
//! registry, advisory locks, append-only version log, file id assignment,
//! cached reads). The capability owns one [`SharedIndex`] cell per
//! instance; cloning the capability shares state with every Harn VM that
//! has been wired against it.
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
//! ### Typed symbol graph (added in #2434)
//!
//! - **`cypher`**: read-only Cypher executor over the typed graph
//!   ([`SymbolGraph`]) — `MATCH ... WHERE ... RETURN` with typed
//!   nodes (Function|Type|Field|EnumCase|Module|Import|CallSite|Macro), typed edges
//!   (CALLS|REFS|IMPORTS|CONTAINS|OVERRIDES, plus `_BY` inverses),
//!   and variable-length hops up to depth 4.
//! - **`branch_overlay`**: per-branch CDC overlay that layers a delta
//!   on top of the base graph; reuses ≥95% of the main index in
//!   storage/CPU for untouched files. See [`BranchOverlay`].
//! - **`freshness`**: per-file hash + mtime comparison against the
//!   indexed snapshot; consumers detect staleness without forcing a
//!   rebuild.
//! - **`repo_map`**: personalized PageRank over the typed graph, rendered
//!   as a token-budgeted symbol map for agent grounding.
//! - **`module_graph`**: the import graph rolled up from files to
//!   directories or modules, for architecture views and diffs.
//!
//! ### Cross-file safe rename (added in #2508)
//!
//! - **`rename_symbol`**: rewrite a symbol across `file | module |
//!   workspace` using the typed graph for symbol resolution and
//!   tree-sitter identifier kinds for safe text spans. Detects
//!   `new_name` shadowing in any rewritten file and aborts before any
//!   write. Routes through staged-fs (#1722) when a `session_id` is
//!   supplied so all touched files succeed or none do.
//! - **`change_signature`**: change a function's parameter list and
//!   rewrite every call site across files (Rust, TypeScript, Python),
//!   refusing value uses, splats, overrides, and removed parameters the
//!   body still reads. Built on [`refactor_core`].
//! - **`extract_function`**: lift an expression, a statement run, or a
//!   closure body into a new function and replace it, and every token-equal
//!   same-file copy, with a call. Built on the shared refactor core.
//! - **`move_symbol`**: move a top-level Rust, TS/JS, or Python
//!   declaration to another module and rewrite imports and qualified
//!   uses in every referencing file, all-or-nothing.
//! - **`language_server`**: whether a rename of a file would reach a
//!   language server, from the lookup the rename uses, without starting one.
//!
//! ## Concurrency model
//!
//! All ops serialise through a single `Arc<Mutex<Option<IndexState>>>` so
//! the IDE editor, eval, and live agent all see one consistent view. The
//! capability is `Send + Sync` so embedders can share it across threads,
//! but the mutex still serialises actual work.

mod agents;
mod builtin_args;
mod builtins;
mod change_signature;
mod cypher;
mod extract;
mod extract_plan;
mod file_facts;
mod file_table;
mod git_head;
mod graph;
mod graph_store;
mod imports;
mod imports_go;
mod imports_swift;
#[cfg(test)]
mod java_binding_tests;
mod java_bindings;
mod language_server;
mod lsp;
mod module_graph;
mod module_index;
mod move_symbol;
mod name_index;
mod overlay;
mod override_family;
mod parallel;
mod postings;
mod readonly;
mod reconcile;
mod refactor_backend;
mod refactor_core;
mod rename;
mod rename_binding;
mod rename_java;
mod rename_reach;
mod rename_rust_residue;
mod repo_map;
mod signature_syntax;
mod snapshot;
mod source_positions;
mod state;
mod symbol_graph;
mod timing;
mod trigram;
mod versions;
pub(crate) mod walker;
mod warm;
mod words;

#[cfg(test)]
mod persistence_tests;

use std::path::Path;
use std::sync::{Arc, Mutex};

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::registry::{BuiltinRegistry, HostlibCapability, RegisteredBuiltin, SyncHandler};

pub use crate::{HarnReferenceInput, HarnReferenceResolver, ResolvedHarnReference};
pub use agents::{AgentId, AgentInfo, AgentRegistry, AgentState, RegistryConfig};
pub use builtins::SharedIndex;
pub use cypher::{CypherError, CypherRow, CypherValue};
pub use file_table::{FileId, IndexedFile, IndexedSymbol};
pub use graph::DepGraph;
pub use imports::{ModuleTarget, ResolutionStrategy};
pub use overlay::{BranchOverlay, OverlayState};
pub use readonly::ReadonlyRoots;
pub use snapshot::{CodeIndexSnapshot, SnapshotMeta, SNAPSHOT_FORMAT_VERSION};
pub use state::{BuildOutcome, ImportCensusRow, IndexState, RefreshOutcome};
pub use symbol_graph::{Edge, EdgeKind, Node, NodeId, NodeKind, SymbolGraph};
pub use trigram::TrigramIndex;
pub use versions::{ChangeRecord, EditOp, VersionEntry, VersionLog, HISTORY_LIMIT};
pub use warm::SessionWarmOutcome;
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
    /// Additive, read-only secondary roots (issue #2403 follow-up). Live
    /// beside the primary slot; query/read_range merge them in but no
    /// mutating builtin ever touches them, so indexing a dependency root
    /// never clobbers the project index.
    readonly: ReadonlyRoots,
    current_agent: Arc<Mutex<Option<AgentId>>>,
    /// Single-flight gate shared by [`Self::warm_session`] and sync rebuild.
    warm: Arc<warm::WarmCoordinator>,
    harn_reference_resolver: Option<HarnReferenceResolver>,
}

impl CodeIndexCapability {
    /// Create a capability with an empty workspace slot. The first
    /// `hostlib_code_index_rebuild` call populates it.
    pub fn new() -> Self {
        Self {
            index: Arc::new(Mutex::new(None)),
            readonly: Arc::new(Mutex::new(Vec::new())),
            current_agent: Arc::new(Mutex::new(None)),
            warm: Arc::new(warm::WarmCoordinator::default()),
            harn_reference_resolver: None,
        }
    }

    /// Install the integration-owned Harn resolver projection.
    pub fn with_harn_reference_resolver(mut self, resolver: HarnReferenceResolver) -> Self {
        self.harn_reference_resolver = Some(resolver);
        self
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

    /// Restore from a previously saved snapshot at the path returned by
    /// [`CodeIndexSnapshot::path_for`]. After restoring, runs
    /// [`IndexState::reap_after_recovery`] so stale agent records and
    /// locks are dropped before the daemon serves traffic.
    ///
    /// Returns `true` on a successful restore, `false` if no snapshot
    /// existed, the format was unrecognised, or the snapshot does not
    /// describe `workspace_root` (wrong checkout). Errors
    /// propagate I/O problems verbatim so callers can decide whether to
    /// fall back to `rebuild`.
    ///
    /// Returns as soon as the reconciled index is published. When
    /// reconciliation changed it, the snapshot is rewritten on a background
    /// thread; [`Self::wait_until_idle`] waits for that write.
    pub fn restore_from_disk(&self, workspace_root: &Path) -> std::io::Result<bool> {
        let _flight = self.warm.begin(workspace_root);
        match restore_shared(
            &self.index,
            self.harn_reference_resolver.as_ref(),
            workspace_root,
        )? {
            Restored::Absent => Ok(false),
            Restored::Published { needs_persist, .. } => {
                if needs_persist {
                    warm::WarmCoordinator::persist_in_background(&self.warm, self.index.clone());
                }
                Ok(true)
            }
        }
    }

    /// Persist the current in-memory state to the path returned by
    /// [`CodeIndexSnapshot::path_for`]. Returns `Ok(false)` when the
    /// capability is empty (nothing to save).
    pub fn persist_to_disk(&self) -> std::io::Result<bool> {
        let root = {
            let guard = self.index.lock().expect("code_index mutex poisoned");
            let Some(state) = guard.as_ref() else {
                return Ok(false);
            };
            state.root.clone()
        };
        let _flight = self.warm.begin(&root);
        // Admission uses the root only for progress and same-root joins.
        // The writer captures the current state and destination afterwards.
        persist_shared(&self.index)
    }

    /// Block until no background warm or snapshot write is in flight for
    /// this capability.
    ///
    /// [`Self::warm_session`] returns [`SessionWarmOutcome::Building`]
    /// without joining the worker, and a restore publishes the index before
    /// rewriting its snapshot. A process that exits in that window can leave
    /// an incomplete cold build, a reconcile that persisted only its completed
    /// checkpoint batches, or the previous snapshot on disk. Short-lived
    /// embedders should call this before exit so the index is persisted.
    /// Returns immediately when nothing is running.
    pub fn wait_until_idle(&self) {
        self.warm.take_and_join_builder();
        self.warm.wait_until_idle();
    }

    /// True when no background warm, rebuild, restore, or snapshot write is
    /// in flight for this capability, so a host can release it without
    /// stranding work that later callers would otherwise join.
    pub fn is_idle(&self) -> bool {
        self.warm.is_idle()
    }
}

/// What [`restore_shared`] did.
enum Restored {
    /// No usable snapshot; nothing was published.
    Absent,
    /// The reconciled index is live. `needs_persist` when reconciliation
    /// changed it, so the snapshot on disk is now stale.
    Published {
        needs_persist: bool,
        outcome: RefreshOutcome,
        files_indexed: usize,
    },
}

/// Body of [`CodeIndexCapability::restore_from_disk`], shared with the
/// background session warm so the load and reconcile run off the caller's
/// thread. Publishes before persisting: rewriting the snapshot is the
/// caller's follow-up, so readers never wait for it.
fn restore_shared(
    index: &SharedIndex,
    resolver: Option<&HarnReferenceResolver>,
    workspace_root: &Path,
) -> std::io::Result<Restored> {
    match CodeIndexSnapshot::load(workspace_root)? {
        Some(snap) => {
            let mut state = IndexState::from_snapshot(snap)?;
            // Always anchor at the caller root. `load` already rejected a
            // snapshot whose recorded root does not match; this keeps
            // the live slot on the requested checkout rather than the
            // stored path string.
            state.root = state::canonicalize(workspace_root);
            state.reap_after_recovery(state::now_unix_ms());
            // A snapshot is a starting point, not a claim of freshness.
            // Reconcile against the files on disk before anyone can read
            // the index. A changed HEAD forces content verification even
            // when size and mtime happen to match.
            let outcome = state.refresh_with_checkpoints(
                None,
                reconcile::CheckpointPolicy::default(),
                |state| {
                    if let Err(error) = state.save_snapshot() {
                        tracing::debug!(
                            target: "harn_hostlib::code_index",
                            %error,
                            root = %state.root.display(),
                            "code-index reconcile checkpoint could not be persisted",
                        );
                    }
                },
            );
            // Resolver-backed Harn references are omitted from the
            // snapshot. Project them once from this host after the file
            // graph is current, so a changed Harn file is not resolved
            // twice against two different graph states.
            state.relink_harn_references(resolver);
            tracing::info!(
                target: "harn_hostlib::code_index",
                root = %state.root.display(),
                files_unchanged = outcome.files_unchanged,
                files_reindexed = outcome.files_reindexed,
                files_added = outcome.files_added,
                files_removed = outcome.files_removed,
                "code-index snapshot restored and reconciled",
            );
            let needs_persist = !outcome.is_noop() || outcome.files_touched_only > 0;
            let files_indexed = state.files.len();
            let mut guard = index.lock().expect("code_index mutex poisoned");
            *guard = Some(state);
            Ok(Restored::Published {
                needs_persist,
                outcome,
                files_indexed,
            })
        }
        None => Ok(Restored::Absent),
    }
}

/// Carry the live index into the next process. A read-only workspace can
/// still use valid in-memory state, so a failure here is logged, not raised.
fn persist_reconciled(index: &SharedIndex) {
    if let Err(error) = persist_shared(index) {
        tracing::debug!(
            target: "harn_hostlib::code_index",
            %error,
            "code-index reconciled snapshot could not be persisted",
        );
    }
}

/// Serializes shared-index captures through their atomic replacement.
/// The capability's warm coordinator separately orders reconciliation and its
/// private checkpoints against earlier shared-index writes.
static PERSIST_ORDER: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn persist_shared(index: &SharedIndex) -> std::io::Result<bool> {
    #[cfg(test)]
    persistence_tests::before_capture(index);
    let _order = PERSIST_ORDER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Hold the index lock only to detach a copy; encode and write without it.
    let (source, root) = {
        let guard = index.lock().expect("code_index mutex poisoned");
        match guard.as_ref() {
            Some(state) => (state.snapshot_source(), state.root.clone()),
            None => return Ok(false),
        }
    };
    #[cfg(test)]
    persistence_tests::after_capture(&root);
    source.encode()?.write(&root)?;
    Ok(true)
}

impl HostlibCapability for CodeIndexCapability {
    fn module_name(&self) -> &'static str {
        "code_index"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        // Workspace queries (original 5). `query` and `read_range` merge in
        // the read-only secondary roots (issue #2403 follow-up), so they
        // capture both the primary and the read-only cells.
        {
            let index = self.index.clone();
            let readonly = self.readonly.clone();
            let handler: SyncHandler =
                Arc::new(move |args| builtins::run_query_merged(&index, Some(&readonly), args));
            registry.register(RegisteredBuiltin {
                name: builtins::BUILTIN_QUERY,
                module: "code_index",
                method: "query",
                handler,
            });
        }
        {
            let index = self.index.clone();
            let warm = self.warm.clone();
            let handler: SyncHandler = {
                let resolver = self.harn_reference_resolver.clone();
                Arc::new(move |args| {
                    warm::run_rebuild_single_flight(&index, &warm, resolver.as_ref(), args)
                })
            };
            registry.register(RegisteredBuiltin {
                name: builtins::BUILTIN_REBUILD,
                module: "code_index",
                method: "rebuild",
                handler,
            });
        }
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

        // Additive read-only secondary roots (issue #2403 follow-up).
        // Captures the read-only cell directly — it never touches the
        // primary index slot.
        {
            let readonly = self.readonly.clone();
            let handler: SyncHandler =
                Arc::new(move |args| readonly::run_add_readonly_roots(&readonly, args));
            registry.register(RegisteredBuiltin {
                name: readonly::BUILTIN_ADD_READONLY_ROOTS,
                module: "code_index",
                method: "add_readonly_roots",
                handler,
            });
        }

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
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_FILE_HASH_SNAPSHOT,
            "file_hash_snapshot",
            builtins::run_file_hash_snapshot,
        );

        // Cached read paths. `read_range` falls back to the read-only
        // secondary roots (issue #2403 follow-up) so a symbol discovered in
        // a dependency root can be read back.
        {
            let index = self.index.clone();
            let readonly = self.readonly.clone();
            let handler: SyncHandler = Arc::new(move |args| {
                builtins::run_read_range_merged(&index, Some(&readonly), args)
            });
            registry.register(RegisteredBuiltin {
                name: builtins::BUILTIN_READ_RANGE,
                module: "code_index",
                method: "read_range",
                handler,
            });
        }
        {
            let index = self.index.clone();
            let resolver = self.harn_reference_resolver.clone();
            let handler: SyncHandler =
                Arc::new(move |args| builtins::run_reindex_file(&index, resolver.as_ref(), args));
            registry.register(RegisteredBuiltin {
                name: builtins::BUILTIN_REINDEX_FILE,
                module: "code_index",
                method: "reindex_file",
                handler,
            });
        }
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

        // Typed symbol graph builtins (issue #2434).
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_CYPHER,
            "cypher",
            builtins::run_cypher,
        );
        register(
            registry,
            self.index.clone(),
            repo_map::BUILTIN,
            "repo_map",
            repo_map::run,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_BRANCH_OVERLAY,
            "branch_overlay",
            builtins::run_branch_overlay,
        );
        register(
            registry,
            self.index.clone(),
            builtins::BUILTIN_FRESHNESS,
            "freshness",
            builtins::run_freshness,
        );
        register(
            registry,
            self.index.clone(),
            module_graph::BUILTIN,
            "module_graph",
            module_graph::run,
        );
        register(
            registry,
            self.index.clone(),
            language_server::BUILTIN,
            "language_server",
            language_server::run,
        );

        // Cross-file safe rename (issue #2508). Builds on the typed
        // symbol graph (#2434) and routes writes through staged-fs
        // (#1722) so all touched files succeed or none do. A language-server
        // rename can wait on a cold server, so its caller must be able to
        // stop it and see its progress.
        register_interruptible(
            registry,
            self.index.clone(),
            rename::BUILTIN,
            "rename_symbol",
            rename::run,
        );
        register(
            registry,
            self.index.clone(),
            change_signature::BUILTIN,
            "change_signature",
            change_signature::run,
        );
        register(
            registry,
            self.index.clone(),
            extract::BUILTIN,
            "extract_function",
            extract::run,
        );
        register(
            registry,
            self.index.clone(),
            move_symbol::BUILTIN,
            "move_symbol",
            move_symbol::run,
        );
    }
}

/// Programmatic entry point for callers that need to compose
/// `rename_symbol` with another hostlib capability while sharing the
/// same in-memory code-index state.
pub(crate) fn run_rename_symbol(
    index: &SharedIndex,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    rename::run(index, args)
}

fn register(
    registry: &mut BuiltinRegistry,
    index: SharedIndex,
    name: &'static str,
    method: &'static str,
    runner: fn(&SharedIndex, &[VmValue]) -> Result<VmValue, HostlibError>,
) {
    registry.register(index_builtin(index, name, method, runner));
}

/// [`register`] for a builtin that can wait on an external process.
fn register_interruptible(
    registry: &mut BuiltinRegistry,
    index: SharedIndex,
    name: &'static str,
    method: &'static str,
    runner: fn(&SharedIndex, &[VmValue]) -> Result<VmValue, HostlibError>,
) {
    registry.register_interruptible(index_builtin(index, name, method, runner));
}

fn index_builtin(
    index: SharedIndex,
    name: &'static str,
    method: &'static str,
    runner: fn(&SharedIndex, &[VmValue]) -> Result<VmValue, HostlibError>,
) -> RegisteredBuiltin {
    let handler: SyncHandler = Arc::new(move |args| runner(&index, args));
    RegisteredBuiltin {
        name,
        module: "code_index",
        method,
        handler,
    }
}

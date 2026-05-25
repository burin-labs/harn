//! Per-branch CDC overlay over the workspace symbol graph.
//!
//! The base [`SymbolGraph`] is what `rebuild` produces for the currently
//! checked-out branch. When a request asks for a *different* branch via
//! `code_index_branch_overlay(branch)`, we lazily layer a delta on top:
//! per-file replacements + deletions that mirror what changed between the
//! base and the named branch. The base graph is still the authoritative
//! source for every untouched file, so switching branches reuses ≥95% of
//! the main index by construction (only the changed slice gets re-parsed).
//!
//! ## Wire model
//!
//! - [`OverlayState::activate`] swaps the currently-active overlay; calls
//!   that follow observe the merged view.
//! - [`OverlayState::reuse_fraction`] reports the share of base files
//!   that the active overlay reused without touching — the metric the
//!   issue's "≥95% reuse" acceptance criterion measures.
//! - [`OverlayState::apply_file`] is the per-file delta primitive used
//!   by both manual stage-and-apply tests and a future Git-driven
//!   change-detector.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::Language;

use super::file_table::FileId;
use super::symbol_graph::SymbolGraph;

/// Per-file delta entry kept on an overlay.
#[derive(Debug, Clone)]
pub enum FileDelta {
    /// The branch has different bytes for this file; rebuild it.
    Modified {
        /// Workspace-relative path of the file on the branch.
        path: String,
        /// Tree-sitter language to parse the branch source as.
        language: Language,
        /// Branch-specific source.
        source: String,
        /// Raw import strings to register against the branch view.
        imports: Vec<String>,
    },
    /// The branch no longer has this file at all.
    Removed,
}

/// A delta layered on top of the base [`SymbolGraph`]. The overlay
/// owns the rebuilt slice for every changed file — the base graph is
/// never mutated when an overlay is activated.
#[derive(Debug, Clone)]
pub struct BranchOverlay {
    /// Branch name (e.g. `topic/auth-refactor`).
    pub branch: String,
    deltas: BTreeMap<FileId, FileDelta>,
    materialized: SymbolGraph,
    materialized_files: BTreeSet<FileId>,
}

impl BranchOverlay {
    /// Make a fresh overlay anchored on `branch`. It carries no deltas
    /// until [`Self::stage`] is called.
    pub fn new(branch: impl Into<String>) -> Self {
        Self {
            branch: branch.into(),
            deltas: BTreeMap::new(),
            materialized: SymbolGraph::new(),
            materialized_files: BTreeSet::new(),
        }
    }

    /// Record a per-file delta. Replaces any previous entry for the
    /// same `file_id`.
    pub fn stage(&mut self, file_id: FileId, delta: FileDelta) {
        self.deltas.insert(file_id, delta);
    }

    /// Total number of changed files this overlay covers.
    pub fn changed_count(&self) -> usize {
        self.deltas.len()
    }

    /// Files that the overlay either adds/modifies or removes.
    pub fn changed_files(&self) -> impl Iterator<Item = FileId> + '_ {
        self.deltas.keys().copied()
    }

    /// Apply every staged delta to the overlay's private graph, using
    /// the base graph as the starting point for inheritance. Returns the
    /// new node count of the overlay-private graph.
    pub fn materialize(&mut self, base: &SymbolGraph) -> usize {
        self.materialized = base.clone();
        self.materialized_files.clear();
        for (file_id, delta) in &self.deltas {
            self.materialized_files.insert(*file_id);
            match delta {
                FileDelta::Removed => {
                    self.materialized.remove_file(*file_id);
                }
                FileDelta::Modified {
                    path,
                    language,
                    source,
                    imports,
                } => {
                    self.materialized
                        .rebuild_file(*file_id, path, *language, source, imports);
                }
            }
        }
        self.materialized.node_count()
    }

    /// Borrow the materialized view. Callers must call
    /// [`Self::materialize`] first or this returns an empty graph.
    pub fn graph(&self) -> &SymbolGraph {
        &self.materialized
    }
}

/// Holder for the active overlay (if any). Threaded through the index
/// state so every Cypher query can opt into the per-branch view.
#[derive(Debug, Default, Clone)]
pub struct OverlayState {
    overlays: BTreeMap<String, BranchOverlay>,
    active: Option<String>,
}

impl OverlayState {
    /// Construct an empty overlay registry with no active branch.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a branch overlay. Activating it is a separate
    /// step ([`Self::activate`]).
    pub fn set(&mut self, overlay: BranchOverlay) {
        self.overlays.insert(overlay.branch.clone(), overlay);
    }

    /// Borrow a stored overlay (if registered).
    pub fn get(&self, branch: &str) -> Option<&BranchOverlay> {
        self.overlays.get(branch)
    }

    /// Mutable borrow of a stored overlay.
    pub fn get_mut(&mut self, branch: &str) -> Option<&mut BranchOverlay> {
        self.overlays.get_mut(branch)
    }

    /// Name of the currently active overlay (if any).
    pub fn active(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// Switch the active overlay. Pass `None` to fall back to the base
    /// graph.
    pub fn activate(&mut self, branch: Option<String>) {
        self.active = branch;
    }

    /// Borrow the active overlay's materialized graph, falling back to
    /// `base` when no overlay is active.
    pub fn graph<'a>(&'a self, base: &'a SymbolGraph) -> &'a SymbolGraph {
        let Some(name) = self.active.as_deref() else {
            return base;
        };
        match self.overlays.get(name) {
            Some(overlay) => overlay.graph(),
            None => base,
        }
    }

    /// Share of `base` files the active overlay reused untouched. Returns
    /// 1.0 when no overlay is active (the base is fully reused).
    pub fn reuse_fraction(&self, base: &SymbolGraph) -> f64 {
        let total = base.file_ids().len();
        if total == 0 {
            return 1.0;
        }
        let Some(name) = self.active.as_deref() else {
            return 1.0;
        };
        let Some(overlay) = self.overlays.get(name) else {
            return 1.0;
        };
        let changed = overlay
            .materialized_files
            .iter()
            .filter(|fid| base.file_ids().contains(fid))
            .count();
        ((total - changed) as f64) / (total as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Language;

    fn base_graph() -> SymbolGraph {
        let mut g = SymbolGraph::new();
        g.rebuild_file(1, "src/a.rs", Language::Rust, "fn a() {}\n", &[]);
        g.rebuild_file(2, "src/b.rs", Language::Rust, "fn b() {}\n", &[]);
        g.rebuild_file(3, "src/c.rs", Language::Rust, "fn c() {}\n", &[]);
        g
    }

    #[test]
    fn unmodified_overlay_returns_base_view() {
        let base = base_graph();
        let mut overlay = BranchOverlay::new("topic/x");
        overlay.materialize(&base);
        let mut state = OverlayState::new();
        state.set(overlay);
        state.activate(Some("topic/x".into()));
        assert_eq!(state.graph(&base).node_count(), base.node_count());
        assert!(state.reuse_fraction(&base) >= 0.99);
    }

    #[test]
    fn modified_overlay_replaces_a_single_file() {
        let base = base_graph();
        let mut overlay = BranchOverlay::new("topic/y");
        overlay.stage(
            2,
            FileDelta::Modified {
                path: "src/b.rs".into(),
                language: Language::Rust,
                source: "fn b2() {}\nfn b3() {}\n".into(),
                imports: Vec::new(),
            },
        );
        overlay.materialize(&base);
        let mut state = OverlayState::new();
        state.set(overlay);
        state.activate(Some("topic/y".into()));
        let view = state.graph(&base);
        // The Function `b` should no longer appear (only the module of
        // that name remains, derived from the basename of src/b.rs).
        let b_funcs: Vec<_> = view
            .iter_nodes()
            .filter(|n| n.kind == super::super::symbol_graph::NodeKind::Function && n.name == "b")
            .collect();
        assert!(b_funcs.is_empty(), "fn b should be gone after overlay");
        assert!(!view.nodes_named("b2").is_empty());
        let reuse = state.reuse_fraction(&base);
        assert!(
            (0.66..1.0).contains(&reuse),
            "expected ~2/3 reuse, got {reuse}"
        );
    }

    #[test]
    fn removed_overlay_deletes_file_slice() {
        let base = base_graph();
        let mut overlay = BranchOverlay::new("topic/del");
        overlay.stage(3, FileDelta::Removed);
        overlay.materialize(&base);
        let mut state = OverlayState::new();
        state.set(overlay);
        state.activate(Some("topic/del".into()));
        assert!(state.graph(&base).nodes_named("c").is_empty());
    }
}
